//! Gate settings, read from `heavy-gate.toml` (flat `key = value` lines).

use std::path::PathBuf;

pub const DEFAULT_CONFIG: &str = "heavy-gate.toml";

#[derive(Debug, Clone)]
pub struct Config {
    /// Heavy commands allowed to run at once across the whole machine.
    pub slots: u32,
    /// A command only starts while at least this much physical memory is available.
    pub min_free_gb: f64,
    /// Parallel jobs exported to the child (CARGO_BUILD_JOBS, CMAKE_BUILD_PARALLEL_LEVEL) when unset.
    pub jobs: u32,
    /// Longest time `heavy run` waits for a slot before giving up with exit code 75.
    pub max_wait_secs: u64,
    /// Slot files and the run log.
    pub state_dir: PathBuf,
    /// "deny" refuses build/compile commands from subagents; "gate" only routes them through the gate.
    pub subagent_builds: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            slots: 3,
            min_free_gb: 6.0,
            jobs: 6,
            max_wait_secs: 3600,
            state_dir: std::env::temp_dir().join("heavy-gate"),
            subagent_builds: "deny".into(),
        }
    }
}

impl Config {
    pub fn load() -> Config {
        let path = std::env::var("HEAVY_GATE_CONFIG").unwrap_or_else(|_| DEFAULT_CONFIG.into());
        let mut cfg = Config::default();
        let Ok(text) = std::fs::read_to_string(&path) else { return cfg };
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim().trim_matches('"').trim_matches('\'');
            match key.trim() {
                "slots" => cfg.slots = value.parse().unwrap_or(cfg.slots).max(1),
                "min_free_gb" => cfg.min_free_gb = value.parse().unwrap_or(cfg.min_free_gb),
                "jobs" => cfg.jobs = value.parse().unwrap_or(cfg.jobs).max(1),
                "max_wait_secs" => cfg.max_wait_secs = value.parse().unwrap_or(cfg.max_wait_secs),
                "state_dir" => cfg.state_dir = PathBuf::from(value.replace("\\\\", "\\")),
                "subagent_builds" => cfg.subagent_builds = value.to_ascii_lowercase(),
                _ => {}
            }
        }
        cfg
    }
}
