//! The CLI uses the same configuration and classifier exported for contract tests.

use heavy_gate::{config, gate, hook};
use std::ffi::OsString;
use std::time::Duration;

const USAGE: &str = "usage:
  heavy run [--label TEXT] [--max-wait SECS] [--no-jobs-env] [--] PROGRAM [ARGS...]
  heavy status
  heavy config
  heavy init
  heavy check [--subagent] [--powershell] COMMAND
  heavy hook
  heavy --version
  heavy --help";

fn main() {
    // Arguments stay as the OS gave them: `heavy run` passes non-UTF-8 file names on unchanged.
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let loaded = config::LoadedConfig::load();
    let command = args.first().and_then(|a| a.to_str());
    if command != Some("hook") {
        loaded.warn();
    }
    let cfg = &loaded.config;
    let code = match command {
        Some("run") => run(cfg, &args[1..]),
        Some("status") if args.len() == 1 => gate::status(cfg),
        Some("config") if args.len() == 1 => loaded.print(),
        Some("init") if args.len() == 1 => match config::init() {
            Ok(path) => {
                println!("{}", path.display());
                0
            }
            Err(e) => {
                eprintln!(
                    "heavy-gate: cannot initialize {}: {e}",
                    loaded.path.display()
                );
                1
            }
        },
        // Bad keys fall back to defaults here as in `run`, silently: a typo in the file must not
        // switch enforcement off. `[hook] disabled = true` is the explicit off switch.
        Some("hook") => hook::run(cfg),
        Some("check") => match args[1..]
            .iter()
            .map(|a| a.to_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
        {
            Some(text) => check(cfg, &text),
            None => usage("check requires a UTF-8 COMMAND"),
        },
        Some("--version" | "version") if args.len() == 1 => {
            println!("heavy {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("--help" | "-h" | "help") if args.len() == 1 => {
            println!("{USAGE}");
            0
        }
        _ => usage("expected a command"),
    };
    std::process::exit(code);
}

fn usage(reason: &str) -> i32 {
    eprintln!("heavy-gate: {reason}\n{USAGE}");
    2
}

fn run(cfg: &config::Config, args: &[OsString]) -> i32 {
    let mut label = None;
    let mut max_wait = cfg.max_wait_secs;
    let mut set_jobs_env = true;
    let mut i = 0;
    while i < args.len() {
        match args[i].to_str() {
            Some("--label") => {
                let Some(text) = args.get(i + 1) else {
                    return usage("--label requires TEXT");
                };
                label = Some(text.to_string_lossy().into_owned());
                i += 2;
            }
            Some("--max-wait") => {
                let Some(seconds) = args
                    .get(i + 1)
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<u64>().ok())
                else {
                    return usage("--max-wait requires a nonnegative integer number of seconds");
                };
                max_wait = seconds;
                i += 2;
            }
            Some("--no-jobs-env") => {
                set_jobs_env = false;
                i += 1;
            }
            Some("--") => {
                i += 1;
                break;
            }
            Some(option) if option.starts_with('-') => return usage("unknown run option"),
            _ => break,
        }
    }
    let Some(program) = args.get(i).filter(|s| !s.is_empty()).cloned() else {
        return usage("run requires PROGRAM");
    };
    let rest = args[i + 1..].to_vec();
    let label = label.unwrap_or_else(|| {
        std::iter::once(&program)
            .chain(&rest)
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(80)
            .collect()
    });
    gate::run(
        cfg,
        gate::RunOptions {
            label,
            max_wait: Duration::from_secs(max_wait),
            set_jobs_env,
            program,
            args: rest,
        },
    )
}

fn check(cfg: &config::Config, args: &[String]) -> i32 {
    let flags = args
        .iter()
        .take_while(|a| *a == "--subagent" || *a == "--powershell")
        .count();
    let subagent = args[..flags].iter().any(|a| a == "--subagent");
    let powershell = args[..flags].iter().any(|a| a == "--powershell");
    let rest = &args[flags..];
    if rest.is_empty() || rest[0].starts_with("--") {
        return usage("check requires COMMAND");
    }
    let command = rest.join(" ");
    if command.trim().is_empty() {
        return usage("check requires a nonempty COMMAND");
    }
    match hook::judge(cfg, &command, subagent, powershell) {
        hook::Verdict::Pass => println!("pass"),
        hook::Verdict::Deny(reason) => println!("deny: {reason}"),
    }
    0
}
