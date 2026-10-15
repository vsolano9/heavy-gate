//! heavy: one machine-wide gate for builds, compiles and renders across every agent session.
//!
//!   heavy run [--label TEXT] [--max-wait SECS] [--no-jobs-env] [--] PROGRAM [ARGS...]
//!   heavy status
//!   heavy hook        (Claude Code PreToolUse hook; reads the payload on stdin)
//!   heavy check [--subagent] [--powershell] COMMAND   (prints the hook verdict for a command line)

mod config;
mod gate;
mod hook;

use std::time::Duration;

const USAGE: &str = "usage:
  heavy run [--label TEXT] [--max-wait SECS] [--no-jobs-env] [--] PROGRAM [ARGS...]
  heavy status
  heavy hook
  heavy check [--subagent] [--powershell] COMMAND";

fn main() {
    let cfg = config::Config::load();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("run") => run(&cfg, &args[1..]),
        Some("status") => gate::status(&cfg),
        Some("hook") => hook::run(&cfg),
        Some("check") => check(&cfg, &args[1..]),
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

fn run(cfg: &config::Config, args: &[String]) -> i32 {
    let mut label = None;
    let mut max_wait = cfg.max_wait_secs;
    let mut set_jobs_env = true;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--label" if i + 1 < args.len() => {
                label = Some(args[i + 1].clone());
                i += 2;
            }
            "--max-wait" if i + 1 < args.len() => {
                max_wait = args[i + 1].parse().unwrap_or(max_wait);
                i += 2;
            }
            "--no-jobs-env" => {
                set_jobs_env = false;
                i += 1;
            }
            "--" => {
                i += 1;
                break;
            }
            _ => break,
        }
    }
    let Some(program) = args.get(i).cloned() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let rest = args[i + 1..].to_vec();
    let label = label.unwrap_or_else(|| {
        let full = std::iter::once(program.as_str()).chain(rest.iter().map(String::as_str)).collect::<Vec<_>>().join(" ");
        full.chars().take(80).collect()
    });
    gate::run(cfg, gate::RunOptions {
        label,
        max_wait: Duration::from_secs(max_wait),
        set_jobs_env,
        program,
        args: rest,
    })
}

fn check(cfg: &config::Config, args: &[String]) -> i32 {
    let flags = args.iter().take_while(|a| *a == "--subagent" || *a == "--powershell").count();
    let subagent = args[..flags].iter().any(|a| a == "--subagent");
    let powershell = args[..flags].iter().any(|a| a == "--powershell");
    let command = args[flags..].join(" ");
    match hook::judge(cfg, &command, subagent, powershell) {
        hook::Verdict::Pass => println!("pass"),
        hook::Verdict::Deny(reason) => println!("deny: {reason}"),
    }
    0
}
