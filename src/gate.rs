//! Machine-wide slots for heavy commands.
//!
//! Each slot is a named Windows mutex (`Local\heavy-gate-slot-N`). The holder keeps it for the
//! life of its child; if the holder dies for any reason Windows abandons the mutex and the next
//! waiter takes it, so a crashed or killed run can never leak a slot. The child runs inside a
//! kill-on-close job object, so killing the holder also kills the whole build tree.

use crate::config::Config;
use std::ffi::c_void;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

pub const EXIT_GATE_TIMEOUT: i32 = 75;
const POLL: Duration = Duration::from_secs(2);
const REPORT_EVERY: Duration = Duration::from_secs(60);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn available_gb() -> f64 {
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return f64::MAX;
    }
    status.ullAvailPhys as f64 / (1024.0 * 1024.0 * 1024.0)
}

struct Slot {
    index: u32,
    handle: HANDLE,
}

impl Slot {
    fn open(index: u32) -> Option<Slot> {
        let name = wide(&format!("Local\\heavy-gate-slot-{index}"));
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return None;
        }
        Some(Slot { index, handle })
    }

    /// Takes the slot without waiting. An abandoned mutex (dead holder) counts as taken.
    fn try_take(&self) -> bool {
        let r = unsafe { WaitForSingleObject(self.handle, 0) };
        r == WAIT_OBJECT_0 || r == WAIT_ABANDONED
    }

    fn release(&self) {
        unsafe { ReleaseMutex(self.handle) };
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

fn slot_file(cfg: &Config, index: u32) -> PathBuf {
    cfg.state_dir.join(format!("slot-{index}.json"))
}

fn describe_holders(cfg: &Config) -> String {
    let mut parts = Vec::new();
    for i in 0..cfg.slots {
        if let Ok(text) = std::fs::read_to_string(slot_file(cfg, i)) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                let age = now_unix().saturating_sub(v["started"].as_u64().unwrap_or(0));
                parts.push(format!(
                    "pid {} `{}` {}m{:02}s",
                    v["pid"],
                    v["label"].as_str().unwrap_or("?"),
                    age / 60,
                    age % 60
                ));
            }
        }
    }
    if parts.is_empty() { "holders unknown".into() } else { parts.join("; ") }
}

fn log_event(cfg: &Config, event: serde_json::Value) {
    // One write per record: `writeln!` on an unbuffered file writes each piece separately, and records of runs
    // that ended together were spliced into each other (runs.jsonl lines 6703-6704, 2026-10-14). An append of
    // a single write lands whole.
    let line = format!("{event}\n");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(cfg.state_dir.join("runs.jsonl"))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Waits for a free slot while enough memory is available, then returns the held slot.
fn acquire(cfg: &Config, label: &str, max_wait: Duration) -> Result<Slot, String> {
    let slots: Vec<Slot> = (0..cfg.slots).filter_map(Slot::open).collect();
    if slots.is_empty() {
        return Err("could not open any gate slot mutex".into());
    }
    let start = Instant::now();
    let mut last_report: Option<Instant> = None;
    loop {
        let free = available_gb();
        let mut reason = String::new();
        if free >= cfg.min_free_gb {
            let mut taken = None;
            for (pos, slot) in slots.iter().enumerate() {
                if slot.try_take() {
                    taken = Some(pos);
                    break;
                }
            }
            if let Some(pos) = taken {
                // Re-check memory now that we hold the slot: another run may have just started.
                if available_gb() >= cfg.min_free_gb {
                    let mut slots = slots;
                    return Ok(slots.swap_remove(pos));
                }
                slots[pos].release();
                reason = format!("only {:.1} GB RAM available (floor {} GB)", available_gb(), cfg.min_free_gb);
            } else {
                reason = format!("all {} slots busy ({})", cfg.slots, describe_holders(cfg));
            }
        }
        if reason.is_empty() {
            reason = format!("only {free:.1} GB RAM available (floor {} GB)", cfg.min_free_gb);
        }
        if start.elapsed() >= max_wait {
            return Err(format!(
                "gave up after {}s waiting: {reason}. Retry later or run it with run_in_background.",
                start.elapsed().as_secs()
            ));
        }
        if last_report.map_or(true, |t| t.elapsed() >= REPORT_EVERY) {
            eprintln!("heavy-gate: waiting to run `{label}`: {reason}");
            last_report = Some(Instant::now());
        }
        std::thread::sleep(POLL);
    }
}

fn kill_on_close_job() -> Option<HANDLE> {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            CloseHandle(job);
            return None;
        }
        Some(job)
    }
}

pub struct RunOptions {
    pub label: String,
    pub max_wait: Duration,
    pub set_jobs_env: bool,
    pub program: String,
    pub args: Vec<String>,
}

/// Resolves a bare program name the way a shell would, so `npm` finds `npm.cmd`
/// (std's Command only finds `.exe` on its own).
fn resolve_program(program: &str) -> PathBuf {
    let given = PathBuf::from(program);
    if given.extension().is_some() && (given.is_file() || given.components().count() > 1) {
        return given;
    }
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|e| !e.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let dirs: Vec<PathBuf> = if given.components().count() > 1 {
        vec![PathBuf::new()]
    } else {
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default()
    };
    for dir in dirs {
        for ext in &exts {
            let candidate = dir.join(format!("{program}{ext}"));
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    given
}

pub fn run(cfg: &Config, opts: RunOptions) -> i32 {
    let _ = std::fs::create_dir_all(&cfg.state_dir);
    let wait_start = Instant::now();
    let slot = match acquire(cfg, &opts.label, opts.max_wait) {
        Ok(slot) => slot,
        Err(msg) => {
            eprintln!("heavy-gate: {msg}");
            log_event(cfg, serde_json::json!({"ts": now_unix(), "event": "timeout", "label": opts.label,
                "waited_s": wait_start.elapsed().as_secs()}));
            return EXIT_GATE_TIMEOUT;
        }
    };
    let waited = wait_start.elapsed().as_secs();
    if waited >= 2 {
        eprintln!("heavy-gate: slot {} acquired after {waited}s", slot.index);
    }
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let holder = serde_json::json!({"slot": slot.index, "pid": std::process::id(), "label": opts.label,
        "cwd": cwd, "started": now_unix()});
    let _ = std::fs::write(slot_file(cfg, slot.index), holder.to_string());

    let mut cmd = Command::new(resolve_program(&opts.program));
    cmd.args(&opts.args);
    if opts.set_jobs_env {
        let jobs = cfg.jobs.to_string();
        for key in ["CARGO_BUILD_JOBS", "CMAKE_BUILD_PARALLEL_LEVEL"] {
            if std::env::var_os(key).is_none() {
                cmd.env(key, &jobs);
            }
        }
    }
    let run_start = Instant::now();
    let code = match cmd.spawn() {
        Ok(mut child) => {
            let job = kill_on_close_job();
            if let Some(job) = job {
                use std::os::windows::io::AsRawHandle;
                unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) };
            }
            let code = match child.wait() {
                Ok(status) => status.code().unwrap_or(1),
                Err(e) => {
                    eprintln!("heavy-gate: wait failed: {e}");
                    1
                }
            };
            if let Some(job) = job {
                unsafe { CloseHandle(job) };
            }
            code
        }
        Err(e) => {
            eprintln!("heavy-gate: could not start `{}`: {e}", opts.program);
            127
        }
    };
    let _ = std::fs::remove_file(slot_file(cfg, slot.index));
    slot.release();
    log_event(cfg, serde_json::json!({"ts": now_unix(), "event": "run", "label": opts.label, "cwd": cwd,
        "slot": slot.index, "waited_s": waited, "ran_s": run_start.elapsed().as_secs(), "exit": code}));
    code
}

pub fn status(cfg: &Config) -> i32 {
    println!("heavy-gate: {} slots, floor {} GB, jobs {}, {:.1} GB RAM available now",
        cfg.slots, cfg.min_free_gb, cfg.jobs, available_gb());
    for i in 0..cfg.slots {
        let Some(slot) = Slot::open(i) else {
            println!("  slot {i}: cannot open");
            continue;
        };
        if slot.try_take() {
            slot.release();
            println!("  slot {i}: free");
        } else {
            let detail = std::fs::read_to_string(slot_file(cfg, i)).unwrap_or_else(|_| "{}".into());
            println!("  slot {i}: busy {detail}");
        }
    }
    0
}
