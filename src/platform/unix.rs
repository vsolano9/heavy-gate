use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

static FLOCK_WARNED: AtomicBool = AtomicBool::new(false);

pub(crate) struct Slot {
    pub(crate) index: u32,
    file: File,
    held: bool,
}

impl Slot {
    pub(crate) fn open(index: u32, state_dir: &Path) -> Option<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            // Never follow a substituted lock symlink or inherit the lock into a child.
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(state_dir.join(format!("slot-{index}.lock")))
            .ok()?;
        Some(Self {
            index,
            file,
            held: false,
        })
    }

    pub(crate) fn try_take(&mut self) -> bool {
        if self.held {
            return true;
        }
        // SAFETY: file owns a valid descriptor for the duration of this call.
        self.held =
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if !self.held {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EWOULDBLOCK)
                && !FLOCK_WARNED.swap(true, Ordering::Relaxed)
            {
                // A filesystem without flock support would otherwise look like every slot is busy.
                eprintln!(
                    "heavy-gate: warning: cannot lock slot {} in the state directory: {error}",
                    self.index
                );
            }
        }
        self.held
    }

    pub(crate) fn release(&mut self) {
        if self.held {
            // SAFETY: the descriptor is still owned by self.
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
            self.held = false;
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(target_os = "linux")]
fn meminfo() -> Option<std::collections::BTreeMap<String, u64>> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    Some(parse_meminfo(&text))
}

#[cfg(target_os = "linux")]
fn parse_meminfo(text: &str) -> std::collections::BTreeMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let (key, rest) = line.split_once(':')?;
            let mut fields = rest.split_whitespace();
            let kb = fields.next()?.parse::<u64>().ok()?;
            if fields.next()? != "kB" {
                return None;
            }
            Some((key.to_string(), kb.checked_mul(1024)?))
        })
        .collect()
}

#[cfg(target_os = "linux")]
pub(crate) fn total_memory_bytes() -> Option<u64> {
    meminfo()?.get("MemTotal").copied()
}

#[cfg(target_os = "linux")]
pub(crate) fn available_memory_bytes() -> Option<u64> {
    let values = meminfo()?;
    values.get("MemAvailable").copied().or_else(|| {
        values
            .get("MemFree")?
            .checked_add(*values.get("Buffers")?)?
            .checked_add(*values.get("Cached")?)
    })
}

#[cfg(target_os = "macos")]
fn sysctl<T: Copy + Default>(name: &std::ffi::CStr) -> Option<T> {
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>();
    // SAFETY: value and size are writable, correctly sized storage; name is NUL terminated.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut value as *mut T).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && size == std::mem::size_of::<T>()).then_some(value)
}

#[cfg(target_os = "macos")]
pub(crate) fn total_memory_bytes() -> Option<u64> {
    sysctl(c"hw.memsize")
}

#[cfg(target_os = "macos")]
pub(crate) fn available_memory_bytes() -> Option<u64> {
    let percent: u32 = sysctl(c"kern.memorystatus_level")?;
    if percent > 100 {
        return None;
    }
    Some((u128::from(total_memory_bytes()?) * u128::from(percent) / 100) as u64)
}

/// Install handlers before admission so signals also cancel a queued command.
pub(crate) struct Signals {
    flags: Vec<(i32, Arc<AtomicBool>)>,
    registrations: Vec<signal_hook::SigId>,
}

impl Signals {
    pub(crate) fn new() -> io::Result<Self> {
        // An inherited SIGCHLD ignore makes the kernel reap the child, so its status would be lost.
        // SAFETY: restoring the default disposition is always valid.
        unsafe { libc::signal(libc::SIGCHLD, libc::SIG_DFL) };
        let mut signals = Self {
            flags: Vec::new(),
            registrations: Vec::new(),
        };
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let flag = Arc::new(AtomicBool::new(false));
            signals
                .registrations
                .push(signal_hook::flag::register(signal, Arc::clone(&flag))?);
            signals.flags.push((signal, flag));
        }
        Ok(signals)
    }

    pub(crate) fn pending(&self) -> Option<i32> {
        self.flags
            .iter()
            .find_map(|(signal, flag)| flag.swap(false, Ordering::Relaxed).then_some(*signal))
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.registrations.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

pub(crate) fn command(program: &std::ffi::OsStr) -> Command {
    Command::new(program)
}

pub(crate) fn run_child(mut command: Command, signals: &Signals) -> io::Result<i32> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: getpid has no preconditions. Capture before fork, not inside the child.
        let parent = unsafe { libc::getpid() };
        // SAFETY: the closure only uses async-signal-safe syscalls and constructs an OS error.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                // Close the race where the parent died between fork and prctl.
                if libc::getppid() != parent {
                    libc::_exit(128 + libc::SIGKILL);
                }
                Ok(())
            });
        }
    }
    // Deliberately inherit the process group and terminal. Ctrl+C reaches both processes.
    let mut child = command.spawn()?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
            }
            Ok(None) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("heavy-gate: could not wait for child: {e}");
                return Ok(1);
            }
        }
        while let Some(signal) = signals.pending() {
            // SAFETY: child has not been reaped, so its PID cannot have been reused.
            // Forwarding a terminal SIGINT twice is harmless for normal default handlers.
            unsafe { libc::kill(child.id() as libc::pid_t, signal) };
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn meminfo_units_and_missing_fields() {
        let values =
            super::parse_meminfo("MemTotal: 1000 kB\nMemFree: 20 kB\nBad: nope kB\nOther: 3 MB\n");
        assert_eq!(values.get("MemTotal"), Some(&1_024_000));
        assert_eq!(values.get("MemFree"), Some(&20_480));
        assert!(!values.contains_key("Bad"));
        assert!(!values.contains_key("Other"));
    }
}
