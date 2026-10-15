use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::Command;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, ReleaseMutex, WaitForSingleObject,
};

pub(crate) struct Slot {
    pub(crate) index: u32,
    handle: HANDLE,
    held: bool,
}

impl Slot {
    pub(crate) fn open(index: u32, state_dir: &Path) -> Option<Self> {
        // Slots belong to a state directory, as the Unix lock files do: every process using the
        // same state_dir competes for the same slots, and a separate state_dir is a separate gate.
        let scope = state_scope(state_dir);
        let name: Vec<u16> = format!("Local\\heavy-gate-{scope:016x}-slot-{index}")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: name is NUL terminated; default security creates a non-inheritable handle.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return None;
        }
        Some(Self {
            index,
            handle,
            held: false,
        })
    }

    pub(crate) fn try_take(&mut self) -> bool {
        if self.held {
            return true;
        }
        // SAFETY: self owns a live mutex handle, on this thread.
        let result = unsafe { WaitForSingleObject(self.handle, 0) };
        self.held = result == WAIT_OBJECT_0 || result == WAIT_ABANDONED;
        self.held
    }

    pub(crate) fn release(&mut self) {
        if self.held {
            // SAFETY: this thread took the mutex and has not released it.
            unsafe { ReleaseMutex(self.handle) };
            self.held = false;
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.release();
        // SAFETY: self owns the handle exclusively.
        unsafe { CloseHandle(self.handle) };
    }
}

/// FNV-1a of the absolute, lowercased state directory (Windows paths are case-insensitive).
/// `std::path::absolute` does not touch the filesystem, so the scope is the same before and after
/// the directory exists (canonicalize would add a `\\?\` prefix only once it does).
fn state_scope(state_dir: &Path) -> u64 {
    let path = std::path::absolute(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    let text = path.to_string_lossy().replace('/', "\\").to_lowercase();
    text.trim_end_matches('\\')
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn memory() -> Option<MEMORYSTATUSEX> {
    // SAFETY: zero is valid initialization; the API requires dwLength before use.
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: status points to initialized writable storage of the advertised length.
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status)
}

pub(crate) fn available_memory_bytes() -> Option<u64> {
    Some(memory()?.ullAvailPhys)
}
pub(crate) fn total_memory_bytes() -> Option<u64> {
    Some(memory()?.ullTotalPhys)
}

pub(crate) struct Signals;
impl Signals {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self)
    }
    pub(crate) fn pending(&self) -> Option<i32> {
        None
    }
}

struct Job(HANDLE);
impl Job {
    fn new() -> io::Result<Self> {
        // SAFETY: null security attributes and name are permitted by the API.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(handle);
        // SAFETY: the POD structure accepts zero initialization.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: info is valid for the specified size and job owns a valid job handle.
        if unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                std::mem::size_of_val(&info) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: this object owns the only non-inherited job handle.
        unsafe { CloseHandle(self.0) };
    }
}

/// Find shell shims (notably npm.cmd) as well as native executables.
fn resolve_program(program: &str) -> PathBuf {
    let given = PathBuf::from(program);
    if given.is_file() || given.extension().is_some() {
        return given;
    }
    let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let dirs: Vec<PathBuf> = if given.components().count() > 1 {
        vec![PathBuf::new()]
    } else {
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default()
    };
    for dir in dirs {
        for extension in extensions.split(';').filter(|e| !e.is_empty()) {
            let candidate = dir.join(format!("{program}{extension}"));
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    given
}

pub(crate) fn command(program: &str) -> Command {
    Command::new(resolve_program(program))
}

pub(crate) fn run_child(mut command: Command, _signals: &Signals) -> io::Result<i32> {
    let job = match Job::new() {
        Ok(job) => Some(job),
        Err(e) => {
            eprintln!("heavy-gate: warning: cannot create cleanup job: {e}; child containment unavailable");
            None
        }
    };
    let mut fallback_job = None;
    if let Some(job) = job {
        // SAFETY: GetCurrentProcess returns a valid pseudo handle; job is live.
        if unsafe { AssignProcessToJobObject(job.0, GetCurrentProcess()) } != 0 {
            // The child inherits membership before executing any code (Windows 8+ nested jobs).
            // Do NOT close this handle in a destructor: closing it kills heavy itself before
            // metadata cleanup, logging, or exit-code propagation. Keep the non-inheritable
            // handle until process exit, when the OS closes it and kills remaining descendants.
            std::mem::forget(job);
        } else {
            eprintln!(
                "heavy-gate: warning: cannot join cleanup job: {}; assigning child after spawn",
                io::Error::last_os_error()
            );
            // Restricted hosts may disallow nested jobs. Best-effort fallback has a race:
            // the spawned child can start grandchildren before assignment succeeds.
            fallback_job = Some(job);
        }
    }
    let mut child = command.spawn()?;
    if let Some(job) = &fallback_job {
        // SAFETY: child and job own valid handles throughout the call.
        if unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) } == 0 {
            eprintln!("heavy-gate: warning: cannot assign child to cleanup job: {}; child containment unavailable", io::Error::last_os_error());
        }
    }
    let code = loop {
        match child.wait() {
            Ok(status) => break status.code().unwrap_or(1),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("heavy-gate: could not wait for child: {e}");
                break 1;
            }
        }
    };
    drop(fallback_job);
    Ok(code)
}
