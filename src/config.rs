//! Lenient, per-key TOML configuration. Invalid settings never prevent a build.

use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const GIB: f64 = 1_073_741_824.0;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HookConfig {
    pub extra_build_programs: Vec<String>,
    pub extra_heavy_programs: Vec<String>,
    pub disabled: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub slots: u32,
    pub min_free_gb: f64,
    pub jobs: u32,
    pub max_wait_secs: u64,
    pub state_dir: PathBuf,
    pub subagent_builds: String,
    pub hook: HookConfig,
}

/// Memory is measured in GiB despite the historical `gb` key.
pub fn auto_defaults(total_bytes: u64, logical_cpus: u32) -> (u32, u32, f64) {
    let ram = total_bytes as f64 / GIB;
    let slots = ((ram / 10.0).floor() as u32).clamp(1, 4);
    let jobs = (logical_cpus / slots).max(1);
    let floor = (ram * 0.15).round().max(2.0);
    (slots, jobs, floor)
}

fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

pub fn config_path() -> PathBuf {
    env_path("HEAVY_GATE_CONFIG").unwrap_or_else(|| {
        let base = if cfg!(windows) {
            env_path("APPDATA").unwrap_or_else(|| home().join("AppData/Roaming"))
        } else {
            env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config"))
        };
        base.join("heavy-gate/heavy-gate.toml")
    })
}

fn state_path() -> PathBuf {
    if cfg!(windows) {
        env_path("LOCALAPPDATA")
            .unwrap_or_else(|| home().join("AppData/Local"))
            .join("heavy-gate")
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support/heavy-gate")
    } else {
        env_path("XDG_STATE_HOME")
            .unwrap_or_else(|| home().join(".local/state"))
            .join("heavy-gate")
    }
}

fn logical_cpus() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get().min(u32::MAX as usize) as u32)
        .unwrap_or(1)
}

impl Default for Config {
    fn default() -> Self {
        let total = crate::platform::total_memory_bytes().unwrap_or(8 * 1_073_741_824);
        let (slots, jobs, min_free_gb) = auto_defaults(total, logical_cpus());
        Self {
            slots,
            jobs,
            min_free_gb,
            max_wait_secs: 3600,
            state_dir: state_path(),
            subagent_builds: "deny".into(),
            hook: HookConfig::default(),
        }
    }
}

pub struct LoadedConfig {
    pub config: Config,
    pub path: PathBuf,
    pub problems: Vec<String>,
    pub sources: BTreeMap<&'static str, &'static str>,
}

fn sources() -> BTreeMap<&'static str, &'static str> {
    [
        ("slots", "auto"),
        ("jobs", "auto"),
        ("min_free_gb", "auto"),
        ("max_wait_secs", "default"),
        ("state_dir", "default"),
        ("subagent_builds", "default"),
        ("hook.extra_build_programs", "default"),
        ("hook.extra_heavy_programs", "default"),
        ("hook.disabled", "default"),
    ]
    .into_iter()
    .collect()
}

fn value<T: DeserializeOwned>(
    loaded: &mut LoadedConfig,
    key: &'static str,
    raw: &toml::Value,
    validate: impl FnOnce(&T) -> bool,
) -> Option<T> {
    if let Ok(parsed) = raw.clone().try_into::<T>() {
        if validate(&parsed) {
            loaded.sources.insert(key, "file");
            return Some(parsed);
        }
    }
    loaded.problems.push(format!(
        "key `{key}`, value `{raw}`: invalid type or range; using default"
    ));
    None
}

fn plain_programs(programs: &[String]) -> bool {
    programs.iter().all(|p| {
        !p.is_empty()
            && p.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.+-".contains(&c))
    })
}

impl LoadedConfig {
    pub fn load() -> Self {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(path, &text),
            Err(e) => {
                let mut loaded = Self::parse(path, "");
                if e.kind() != io::ErrorKind::NotFound {
                    loaded
                        .problems
                        .push(format!("key `<file>`, value `<unreadable>`: {e}"));
                }
                loaded
            }
        }
    }

    pub fn parse(path: PathBuf, text: &str) -> Self {
        let mut loaded = Self {
            config: Config::default(),
            path,
            problems: Vec::new(),
            sources: sources(),
        };
        let table = match text.parse::<toml::Table>() {
            Ok(table) => table,
            Err(e) => {
                loaded
                    .problems
                    .push(format!("key `<document>`, value `<invalid TOML>`: {e}"));
                return loaded;
            }
        };
        for (key, raw) in table {
            match key.as_str() {
                "slots" => {
                    if let Some(v) =
                        value(&mut loaded, "slots", &raw, |v: &u32| (1..=1024).contains(v))
                    {
                        loaded.config.slots = v;
                    }
                }
                "jobs" => {
                    if let Some(v) = value(&mut loaded, "jobs", &raw, |v: &u32| *v > 0) {
                        loaded.config.jobs = v;
                    }
                }
                "min_free_gb" => {
                    if let Some(v) = value(&mut loaded, "min_free_gb", &raw, |v: &f64| {
                        v.is_finite() && *v >= 0.0
                    }) {
                        loaded.config.min_free_gb = v;
                    }
                }
                "max_wait_secs" => {
                    if let Some(v) = value(&mut loaded, "max_wait_secs", &raw, |_: &u64| true) {
                        loaded.config.max_wait_secs = v;
                    }
                }
                "state_dir" => {
                    if let Some(v) = value(&mut loaded, "state_dir", &raw, |v: &String| {
                        !v.is_empty() && !v.contains('\0')
                    }) {
                        loaded.config.state_dir = PathBuf::from(v);
                    }
                }
                "subagent_builds" => {
                    if let Some(v) = value(&mut loaded, "subagent_builds", &raw, |v: &String| {
                        v == "deny" || v == "gate"
                    }) {
                        loaded.config.subagent_builds = v;
                    }
                }
                "hook" => {
                    if let Some(table) = raw.as_table() {
                        for (key, raw) in table {
                            match key.as_str() {
                                "extra_build_programs" => {
                                    if let Some(v) = value(
                                        &mut loaded,
                                        "hook.extra_build_programs",
                                        raw,
                                        |v: &Vec<String>| plain_programs(v),
                                    ) {
                                        loaded.config.hook.extra_build_programs = v;
                                    }
                                }
                                "extra_heavy_programs" => {
                                    if let Some(v) = value(
                                        &mut loaded,
                                        "hook.extra_heavy_programs",
                                        raw,
                                        |v: &Vec<String>| plain_programs(v),
                                    ) {
                                        loaded.config.hook.extra_heavy_programs = v;
                                    }
                                }
                                "disabled" => {
                                    if let Some(v) =
                                        value(&mut loaded, "hook.disabled", raw, |_: &bool| true)
                                    {
                                        loaded.config.hook.disabled = v;
                                    }
                                }
                                _ => loaded.problems.push(format!(
                                    "key `hook.{key}`, value `{raw}`: unknown key; ignored"
                                )),
                            }
                        }
                    } else {
                        loaded.problems.push(format!(
                            "key `hook`, value `{raw}`: expected a table; using defaults"
                        ));
                    }
                }
                _ => loaded
                    .problems
                    .push(format!("key `{key}`, value `{raw}`: unknown key; ignored")),
            }
        }
        // An explicit slot count also sizes the default per-command parallelism.
        if loaded.sources["jobs"] != "file" {
            loaded.config.jobs = (logical_cpus() / loaded.config.slots).max(1);
        }
        loaded
    }

    pub fn warn(&self) {
        for problem in &self.problems {
            eprintln!(
                "heavy-gate: {}: {}",
                self.path.display(),
                problem.replace(['\r', '\n'], " ")
            );
        }
    }

    pub fn print(&self) -> i32 {
        println!("config: {}", self.path.display());
        let cfg = &self.config;
        let effective = [
            ("slots", cfg.slots.to_string()),
            ("min_free_gb", cfg.min_free_gb.to_string()),
            ("jobs", cfg.jobs.to_string()),
            ("max_wait_secs", cfg.max_wait_secs.to_string()),
            ("state_dir", format!("{:?}", cfg.state_dir)),
            ("subagent_builds", format!("{:?}", cfg.subagent_builds)),
            (
                "hook.extra_build_programs",
                format!("{:?}", cfg.hook.extra_build_programs),
            ),
            (
                "hook.extra_heavy_programs",
                format!("{:?}", cfg.hook.extra_heavy_programs),
            ),
            ("hook.disabled", cfg.hook.disabled.to_string()),
        ];
        for (key, value) in effective {
            println!("{key} = {value} ({})", self.sources[key]);
        }
        i32::from(!self.problems.is_empty())
    }
}

pub fn init() -> io::Result<PathBuf> {
    let path = config_path();
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        create_state_dir(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    if let Err(e) = file
        .write_all(include_bytes!("../heavy-gate.example.toml"))
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
}

pub(crate) fn create_state_dir(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_sizing() {
        for (ram, cpus, expected) in [
            (0, 0, (1, 1, 2.0)),
            (8, 8, (1, 8, 2.0)),
            (20, 3, (2, 1, 3.0)),
            (32, 16, (3, 5, 5.0)),
            (40, 32, (4, 8, 6.0)),
            (128, 2, (4, 1, 19.0)),
        ] {
            assert_eq!(auto_defaults(ram * 1_073_741_824, cpus), expected);
        }
    }

    #[test]
    fn toml_strings_tables_and_sources() {
        let loaded = LoadedConfig::parse(
            "settings.toml".into(),
            r#"
slots = 2
jobs = 3
min_free_gb = 1.5
max_wait_secs = 0
state_dir = 'a directory/#state'
subagent_builds = 'gate'
[hook]
extra_build_programs = ['builder++', 'custom-build']
extra_heavy_programs = ['renderer']
disabled = true
"#,
        );
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.config.state_dir, PathBuf::from("a directory/#state"));
        assert_eq!(loaded.config.slots, 2);
        assert_eq!(loaded.config.jobs, 3);
        assert_eq!(loaded.config.max_wait_secs, 0);
        assert_eq!(loaded.config.min_free_gb, 1.5);
        assert!(loaded.config.hook.disabled);
        assert!(loaded.sources.values().all(|s| *s == "file"));
    }

    #[test]
    fn invalid_keys_fall_back_individually() {
        let loaded = LoadedConfig::parse(
            "settings.toml".into(),
            r#"
slots = 0
jobs = 'four'
min_free_gb = nan
max_wait_secs = -1
subagent_builds = 'allow'
state_dir = ''
typo = 7
[hook]
disabled = 'yes'
extra_build_programs = ['path/to/build']
extra_heavy_programs = ['okay', 3]
unknown = false
"#,
        );
        assert_eq!(loaded.problems.len(), 11);
        assert!(loaded.sources.values().all(|s| *s != "file"));
        assert!(!loaded.config.hook.disabled);
        assert!(loaded.config.hook.extra_build_programs.is_empty());
    }

    #[test]
    fn syntax_error_uses_all_defaults() {
        let loaded = LoadedConfig::parse("broken.toml".into(), "slots = [");
        assert_eq!(loaded.problems.len(), 1);
        assert_eq!(loaded.sources["slots"], "auto");
    }

    #[test]
    fn integer_memory_floor_is_valid_toml() {
        let loaded = LoadedConfig::parse(
            "settings.toml".into(),
            "min_free_gb = 4\njobs = 7\nslots = 2",
        );
        assert!(loaded.problems.is_empty());
        assert_eq!(loaded.config.min_free_gb, 4.0);
    }

    #[test]
    fn valid_keys_survive_neighboring_errors() {
        let loaded = LoadedConfig::parse("settings.toml".into(), "slots = 2\njobs = 0\nmin_free_gb = 0\nmax_wait_secs = 12\n[hook]\ndisabled = true\nmisspelled = 42");
        assert_eq!(loaded.problems.len(), 2);
        assert_eq!(loaded.config.slots, 2);
        assert_eq!(loaded.config.min_free_gb, 0.0);
        assert_eq!(loaded.config.max_wait_secs, 12);
        assert!(loaded.config.hook.disabled);
        assert_eq!(loaded.sources["jobs"], "auto");
        assert_eq!(loaded.config.jobs, (logical_cpus() / 2).max(1));
        assert!(loaded
            .problems
            .iter()
            .any(|p| p.contains("hook.misspelled") && p.contains("42")));
    }

    #[test]
    fn reject_non_table_hook_and_out_of_range_values() {
        for text in [
            "hook = false",
            "slots = 1025",
            "jobs = 4294967296",
            "min_free_gb = -0.1",
            "min_free_gb = inf",
            "slots = 1.5",
            "max_wait_secs = '30'",
        ] {
            let loaded = LoadedConfig::parse("settings.toml".into(), text);
            assert_eq!(loaded.problems.len(), 1, "{text}: {:?}", loaded.problems);
            assert!(loaded.sources.values().all(|source| *source != "file"));
        }
    }

    #[test]
    fn fractional_ram_rounds_slots_down_and_floor_to_nearest() {
        assert_eq!(auto_defaults(19 * 1_073_741_824, 7), (1, 7, 3.0));
        assert_eq!(auto_defaults(30 * 1_073_741_824, 11), (3, 3, 5.0));
        assert_eq!(auto_defaults(39 * 1_073_741_824, 2), (3, 1, 6.0));
    }

    #[test]
    fn shipped_example_parses_without_problems() {
        let loaded = LoadedConfig::parse(
            "example.toml".into(),
            include_str!("../heavy-gate.example.toml"),
        );
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    }
}
