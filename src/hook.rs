//! Claude Code PreToolUse hook for the Bash and PowerShell tools.
//!
//! - Subagents may not run builds or compiles: denied with a hand-back message.
//! - Everything heavy (builds, compiles, Blender, video renders) must run through `heavy run --`;
//!   an unwrapped call is denied with the exact wrapped form to use instead.
//! - Anything else passes silently.
//!
//! A program counts only where a command starts (line start or after `;`, `&&`, `||`, `|`, `&`, `(`, `{`),
//! so text inside commit messages, greps and echo strings does not trigger it. In PowerShell a quoted string
//! there is an expression that prints itself, not a program, unless the call operator `&` runs it.

use crate::config::Config;
use regex::Regex;
use std::io::Read;

pub const HEAVY_PATH: &str = "heavy";

/// Start of a command, plus PowerShell `&`, `cmd /c`, bash `VAR=x` and `env`/`time` prefixes.
const START: &str = r#"(?:^|[\n;|&({]|\b(?:then|do|else)\b)\s*(?:&\s*)?(?:(?:cmd(?:\.exe)?\s+//?[ck]|time|env|nice|start(?:\s+/\w+)*)\s+)*(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*(?:['"][^'"\n]*[\\/]|['"]?(?:[^\s'"]*[\\/])?)"#;
/// End of a program name: optional Windows extension, optional closing quote, then a separator.
const END: &str = r#"(?:\.exe|\.cmd|\.bat)?['"]?(?:\s|$|[;|&)}])"#;

const BUILD_PROGRAMS: &[&str] = &[
    r"cargo@(?:\s+\+\S+)?\s+(?:build|b|run|r|test|t|check|c|clippy|bench|install|doc|rustc|nextest|xtask|tauri\s+build)\b",
    r"(?:rustc|cl|clang|clang\+\+|clang-cl|gcc|g\+\+|c\+\+|msbuild|devenv|ninja|make|nmake|mingw32-make|jom|gradlew?|mvn|xcodebuild|tsc|webpack|wasm-pack)",
    r"dotnet@\s+(?:build|run|test|publish|pack|watch|msbuild)\b",
    r"cmake@\s+--build\b",
    r"(?:npm|pnpm|yarn|bun)@\s+(?:run\s+)?(?:build|test|typecheck|tsc)",
    r"npx@\s+(?:-\S+\s+)*(?:vite\s+build|tsc|next\s+build|webpack|turbo|tauri\s+build)",
    r"(?:vite|next|astro|nuxt|tauri)@\s+build\b",
    r"turbo@\s+(?:run\s+)?build\b",
    r"go@\s+(?:build|test|run|install|vet)\b",
    r"swift@\s+(?:build|test|run)\b",
    r"zig@\s+(?:build|cc|c\+\+)\b",
    // Build scripts run directly or through a shell or interpreter.
    r#"(?:(?:pwsh|powershell|bash|sh|python3?|py)@(?:\s+-\S+)*\s+['"]?(?:[^\s'"]*[\\/])?)?[^\s'"\\/]*build[\w.-]*\.(?:ps1|sh|cmd|bat|py)"#,
];

const HEAVY_PROGRAMS: &[&str] = &[
    r"blender",
    r"(?:npx@\s+)?(?:hyperframes|remotion)@\s+render\b",
];

/// `@` in a pattern marks where a program name ends and its arguments begin.
const NAME_END: &str = r#"(?:\.exe|\.cmd|\.bat)?['"]?"#;

fn compile(programs: &[&str]) -> Regex {
    let alternation = programs.join("|").replace('@', NAME_END);
    Regex::new(&format!(r"(?i){START}(?:{alternation}){END}")).expect("hook regex")
}

/// First match that is a real run, skipping version and help probes like `rustc --version`.
fn first_run(re: &Regex, command: &str) -> Option<String> {
    re.find_iter(command)
        .find(|m| {
            let next = command[m.end()..]
                .split(|c: char| c.is_whitespace() || ";&|".contains(c))
                .find(|t| !t.is_empty())
                .unwrap_or("");
            !["--version", "-V", "-vV", "--help", "-h", "/?"].contains(&next)
        })
        .map(|m| m.as_str().trim_matches(|c: char| c.is_whitespace() || ";|&(){}".contains(c)).to_string())
}

/// The commands handed to `heavy run`, one per occurrence.
fn gated_targets(command: &str) -> Vec<String> {
    let re = Regex::new(
        r#"(?i)heavy(?:\.exe)?['"]?\s+run\s+(?:(?:--label\s+(?:'[^']*'|"[^"]*"|\S+)|--max-wait\s+\S+|--no-jobs-env)\s+)*(?:--\s+)?([^\n]*)"#,
    )
    .expect("gate regex");
    re.captures_iter(command).map(|c| c[1].to_string()).collect()
}

/// Blanks text that is data rather than commands, so it cannot look like a command start:
/// heredoc and here-string bodies, and quoted arguments. A quoted string stays visible where it
/// is the program itself (`& "C:\...\cargo.exe" build`) or the body of `-c`/`-Command`/`/c`.
/// `powershell` says which shell reads `command`: there a quoted string at a command start is a
/// program only after the call operator (`"make sure ..."` just prints), while Bash and cmd run it.
fn mask_data(command: &str, powershell: bool) -> String {
    let heredoc = Regex::new(r#"<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?"#).expect("heredoc regex");
    let mut text = command.to_string();
    let mut from = 0;
    while let Some(caps) = heredoc.captures(&text[from..]) {
        let delimiter = caps[1].to_string();
        let marker_end = from + caps.get(0).unwrap().end();
        let Some(body_start) = text[marker_end..].find('\n').map(|i| marker_end + i + 1) else { break };
        let mut end = text.len();
        let mut pos = body_start;
        for line in text[body_start..].split_inclusive('\n') {
            if line.trim() == delimiter {
                end = pos;
                break;
            }
            pos += line.len();
        }
        let blank: String = text[body_start..end].chars().map(|c| if c == '\n' { '\n' } else { ' ' }).collect();
        text.replace_range(body_start..end, &blank);
        from = end.min(text.len());
    }

    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // PowerShell here-string: @' ... '@ or @" ... "@ with the closer at a line start.
        if c == '@' && i + 2 < chars.len() && (chars[i + 1] == '\'' || chars[i + 1] == '"') && chars[i + 2] == '\n' {
            let close: String = ['\n', chars[i + 1], '@'].iter().collect();
            let rest: String = chars[i..].iter().collect();
            let len = rest.find(&close).map_or(rest.chars().count(), |b| rest[..b + close.len()].chars().count());
            out.push_str("@''@");
            i += len;
            continue;
        }
        if c == '\'' || c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                if c == '"' && (chars[j] == '\\' || chars[j] == '`') {
                    j += 1;
                }
                j += 1;
            }
            let segment: String = chars[i..j.min(chars.len() - 1) + 1].iter().collect();
            let before = out.trim_end();
            let lower = before.to_ascii_lowercase();
            let program = if powershell {
                before.ends_with('&') && !before.ends_with("&&")
            } else {
                before.is_empty() || before.ends_with(|ch: char| ";|&({\n".contains(ch))
            };
            // cmd's /c and /k, spelled //c and //k in Git Bash so MSYS leaves them alone.
            let cmd = [" /c", " /k", " //c", " //k"].iter().any(|f| lower.ends_with(f));
            let script = cmd || [" -c", " -lc", " -ic", " -command"].iter().any(|f| lower.ends_with(f));
            if cmd {
                // cmd takes the rest of the line as its command and, when it starts with a quote, drops that
                // quote and the line's last one: `cmd /c ""C:\a b\x.exe" -y"` runs `"C:\a b\x.exe" -y`.
                let line_end = chars[i..].iter().position(|&ch| ch == '\n').map_or(chars.len(), |p| i + p);
                let line: String = chars[i..line_end].iter().collect();
                let body = line.trim_end();
                let inner = body.strip_prefix('"').and_then(|b| b.strip_suffix('"')).unwrap_or(body);
                out.push_str("; ");
                out.push_str(&mask_data(inner, false));
                out.push_str(" ;");
                i = line_end;
                continue;
            }
            if script {
                // The body is itself a command line, read by the shell the flag belongs to: open it as a
                // new command.
                let shell = lower.rsplit_once(' ').map_or("", |(head, _)| head).trim_end_matches(['\'', '"']);
                let inner_powershell = [r"pwsh", r"pwsh.exe", r"powershell", r"powershell.exe"]
                    .iter()
                    .any(|name| shell.ends_with(name));
                let inner = segment.trim_start_matches(c).trim_end_matches(c);
                out.push_str("; ");
                out.push_str(&mask_data(inner, inner_powershell));
                out.push_str(" ;");
            } else if program {
                out.push_str(&segment);
            } else {
                out.push(c);
                out.push(c);
            }
            i = j + 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

pub enum Verdict {
    Pass,
    Deny(String),
}

pub fn judge(cfg: &Config, command: &str, is_subagent: bool, powershell: bool) -> Verdict {
    let build_re = compile(BUILD_PROGRAMS);
    let visible = mask_data(command, powershell);
    let build = first_run(&build_re, &visible);
    let heavy = first_run(&compile(HEAVY_PROGRAMS), &visible);
    let targets = gated_targets(&visible);
    let wrapped = !targets.is_empty();
    let wrapped_build = targets.iter().find_map(|t| first_run(&build_re, t));

    if is_subagent && cfg.subagent_builds == "deny" && (build.is_some() || wrapped_build.is_some()) {
        let what = build.or(wrapped_build).unwrap_or_default();
        return Verdict::Deny(format!(
            "Machine-wide memory rule: subagents do not compile, build or run test builds; only the lead agent does \
             (several sessions share this machine and parallel builds can exhaust RAM). Blocked: `{what}`. \
             Do not retry it another way. Finish your edits, then end your report with what needs building or \
             compiling, the exact command and what to check in its output; the lead will run it. If this command \
             is not actually a build, rewrite it so the tool name does not start a command."
        ));
    }
    if wrapped || (build.is_none() && heavy.is_none()) {
        return Verdict::Pass;
    }
    let what = build.or(heavy).unwrap_or_default();
    Verdict::Deny(format!(
        "Machine-wide memory rule: heavy commands (builds, compiles, Blender, video renders) run through the shared \
         gate so parallel sessions cannot exhaust RAM. Blocked: `{what}`. Rerun the same command with only the heavy \
         part prefixed by `heavy run -- `, for example `heavy run -- cargo build --release` or \
         `cd app && heavy run -- npm run build` (full path if `heavy` is not on PATH: {HEAVY_PATH}). The gate may \
         queue when the machine is busy; use run_in_background for long builds. `heavy status` shows the queue."
    ))
}

/// Reads the PreToolUse payload from stdin and prints a decision only when denying.
pub fn run(cfg: &Config) -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&input) else { return 0 };
    let tool = payload["tool_name"].as_str().unwrap_or("");
    if tool != "Bash" && tool != "PowerShell" {
        return 0;
    }
    let Some(command) = payload["tool_input"]["command"].as_str() else { return 0 };
    let is_subagent = payload["agent_id"].as_str().is_some_and(|s| !s.is_empty());
    if let Verdict::Deny(reason) = judge(cfg, command, is_subagent, tool == "PowerShell") {
        let out = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        });
        println!("{out}");
    }
    0
}
