//! Shared admission, holder metadata, and local run records.

use crate::config::{create_state_dir, Config};
use crate::platform::{self, Signals, Slot};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Once;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const EXIT_GATE_TIMEOUT: i32 = 75;
const GIB: f64 = 1_073_741_824.0;
const POLL: Duration = Duration::from_secs(2);
const REPORT_EVERY: Duration = Duration::from_secs(60);
static MEMORY_WARNING: Once = Once::new();

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn available() -> Option<u64> {
    let bytes = platform::available_memory_bytes();
    if bytes.is_none() {
        MEMORY_WARNING.call_once(|| eprintln!(
            "heavy-gate: warning: cannot read available memory; treating the memory floor as satisfied"
        ));
    }
    bytes
}

fn meets_floor(cfg: &Config, bytes: Option<u64>) -> bool {
    bytes.is_none_or(|b| b as f64 / GIB >= cfg.min_free_gb)
}

fn slot_file(cfg: &Config, index: u32) -> PathBuf {
    cfg.state_dir.join(format!("slot-{index}.json"))
}

fn holder_detail(cfg: &Config, index: u32) -> String {
    let holder = std::fs::read_to_string(slot_file(cfg, index))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let Some(holder) = holder else {
        return "holder unknown".into();
    };
    let age = now_unix().saturating_sub(holder["started"].as_u64().unwrap_or(now_unix()));
    format!(
        "pid {} label {:?} cwd {:?} age {}m{:02}s",
        holder["pid"],
        holder["label"].as_str().unwrap_or("?"),
        holder["cwd"].as_str().unwrap_or("?"),
        age / 60,
        age % 60
    )
}

fn describe_holders(cfg: &Config) -> String {
    (0..cfg.slots)
        .filter_map(|index| {
            let mut slot = Slot::open(index, &cfg.state_dir)?;
            if slot.try_take() {
                return None;
            }
            Some(format!("slot {index}: {}", holder_detail(cfg, index)))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options
}

fn log_event(cfg: &Config, event: serde_json::Value) {
    let result = (|| -> io::Result<()> {
        // Reserved outside the configurable slot range, with the same kernel lifetime rules.
        // Serialize append writes, including short writes, across independent wrappers.
        let mut lock = Slot::open(u32::MAX, &cfg.state_dir)
            .ok_or_else(|| io::Error::other("cannot open run-log lock"))?;
        let start = Instant::now();
        while !lock.try_take() {
            if start.elapsed() >= Duration::from_secs(2) {
                return Err(io::Error::other("run-log lock busy"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let line = format!("{event}\n");
        private_options()
            .create(true)
            .append(true)
            .open(cfg.state_dir.join("runs.jsonl"))?
            .write_all(line.as_bytes())
    })();
    if let Err(e) = result {
        eprintln!("heavy-gate: warning: cannot append run log: {e}");
    }
}

struct AdmissionError {
    code: i32,
    reason: String,
}

fn acquire(
    cfg: &Config,
    label: &str,
    max_wait: Duration,
    signals: &Signals,
) -> Result<Slot, AdmissionError> {
    let mut slots: Vec<Slot> = (0..cfg.slots)
        .filter_map(|i| Slot::open(i, &cfg.state_dir))
        .collect();
    if slots.len() != cfg.slots as usize {
        return Err(AdmissionError {
            code: 1,
            reason:
                "cannot open all gate slots; check state-directory permissions and OS resources"
                    .into(),
        });
    }
    let start = Instant::now();
    let mut last_report: Option<Instant> = None;
    loop {
        if let Some(signal) = signals.pending() {
            return Err(AdmissionError {
                code: 128 + signal,
                reason: format!(
                    "admission interrupted by signal {signal}; command was not started"
                ),
            });
        }
        let free = available();
        let reason = if meets_floor(cfg, free) {
            if let Some(pos) = slots.iter_mut().position(Slot::try_take) {
                let rechecked = available();
                if meets_floor(cfg, rechecked) {
                    return Ok(slots.swap_remove(pos));
                }
                slots[pos].release();
                memory_reason(cfg, rechecked)
            } else {
                format!("all {} slots busy ({})", cfg.slots, describe_holders(cfg))
            }
        } else {
            memory_reason(cfg, free)
        };
        if start.elapsed() >= max_wait {
            return Err(AdmissionError {
                code: EXIT_GATE_TIMEOUT,
                reason: format!(
                    "gave up after {}s waiting: {reason}",
                    start.elapsed().as_secs()
                ),
            });
        }
        if last_report.is_none_or(|t| t.elapsed() >= REPORT_EVERY) {
            eprintln!("heavy-gate: waiting to run {label:?}: {reason}");
            last_report = Some(Instant::now());
        }
        let pause = POLL.min(max_wait.saturating_sub(start.elapsed()));
        let sleep_start = Instant::now();
        while sleep_start.elapsed() < pause {
            if let Some(signal) = signals.pending() {
                return Err(AdmissionError {
                    code: 128 + signal,
                    reason: format!(
                        "admission interrupted by signal {signal}; command was not started"
                    ),
                });
            }
            std::thread::sleep(
                Duration::from_millis(50).min(pause.saturating_sub(sleep_start.elapsed())),
            );
        }
    }
}

fn memory_reason(cfg: &Config, bytes: Option<u64>) -> String {
    format!(
        "only {:.1} GiB RAM available (floor {} GiB)",
        bytes.unwrap_or(0) as f64 / GIB,
        cfg.min_free_gb
    )
}

pub struct RunOptions {
    pub label: String,
    pub max_wait: Duration,
    pub set_jobs_env: bool,
    pub program: std::ffi::OsString,
    pub args: Vec<std::ffi::OsString>,
}

pub fn run(cfg: &Config, opts: RunOptions) -> i32 {
    if let Err(e) = create_state_dir(&cfg.state_dir) {
        eprintln!(
            "heavy-gate: cannot create state directory {}: {e}",
            cfg.state_dir.display()
        );
        return 1;
    }
    let signals = match Signals::new() {
        Ok(signals) => signals,
        Err(e) => {
            eprintln!("heavy-gate: cannot install signal handlers: {e}");
            return 1;
        }
    };
    let wait_start = Instant::now();
    let mut slot = match acquire(cfg, &opts.label, opts.max_wait, &signals) {
        Ok(slot) => slot,
        Err(e) => {
            eprintln!("heavy-gate: {}", e.reason);
            log_event(
                cfg,
                serde_json::json!({"ts": now_unix(),
                "event": if e.code == EXIT_GATE_TIMEOUT { "timeout" } else { "admission_error" },
                "label": opts.label, "pid": std::process::id(), "exit": e.code,
                "waited_s": wait_start.elapsed().as_secs()}),
            );
            return e.code;
        }
    };
    let waited = wait_start.elapsed().as_secs();
    if waited >= 2 {
        eprintln!("heavy-gate: slot {} acquired after {waited}s", slot.index);
    }
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let holder = serde_json::json!({"slot": slot.index, "pid": std::process::id(), "label": opts.label,
        "cwd": cwd, "started": now_unix()});
    if let Err(e) = private_options()
        .create(true)
        .write(true)
        .truncate(true)
        .open(slot_file(cfg, slot.index))
        .and_then(|mut f| f.write_all(holder.to_string().as_bytes()))
    {
        eprintln!("heavy-gate: warning: cannot write holder metadata: {e}");
    }
    let mut cmd = platform::command(&opts.program);
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
    let code = if let Some(signal) = signals.pending() {
        128 + signal
    } else {
        match platform::run_child(cmd, &signals) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("heavy-gate: could not start {:?}: {e}", opts.program);
                127
            }
        }
    };
    // Remove metadata before unlocking so an exiting holder cannot erase its successor.
    let _ = std::fs::remove_file(slot_file(cfg, slot.index));
    slot.release();
    log_event(
        cfg,
        serde_json::json!({"ts": now_unix(), "event": "run", "label": opts.label, "cwd": cwd,
        "pid": std::process::id(), "slot": slot.index, "waited_s": waited,
        "ran_s": run_start.elapsed().as_secs(), "exit": code}),
    );
    code
}

pub fn status(cfg: &Config) -> i32 {
    if let Err(e) = create_state_dir(&cfg.state_dir) {
        eprintln!(
            "heavy-gate: cannot open state directory {}: {e}",
            cfg.state_dir.display()
        );
        return 1;
    }
    let memory = |bytes: Option<u64>| {
        bytes.map_or_else(
            || "unknown".into(),
            |b| format!("{:.1} GiB", b as f64 / GIB),
        )
    };
    println!(
        "heavy-gate: {} slots, floor {} GiB, jobs {}",
        cfg.slots, cfg.min_free_gb, cfg.jobs
    );
    println!(
        "memory: {} available / {} total",
        memory(available()),
        memory(platform::total_memory_bytes())
    );
    let mut code = 0;
    for index in 0..cfg.slots {
        let Some(mut slot) = Slot::open(index, &cfg.state_dir) else {
            println!("  slot {index}: cannot open");
            code = 1;
            continue;
        };
        if slot.try_take() {
            println!("  slot {index}: free");
        } else {
            println!("  slot {index}: busy {}", holder_detail(cfg, index));
        }
    }
    code
}
