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

/// Start of a command, plus PowerShell `&`, `cmd /c`, bash `VAR=x` and `env`/`time` prefixes.
const START: &str = r#"(?:^|[\n;|&({]|\b(?:then|do|else)\b)\s*(?:&\s*)?(?:(?:cmd(?:\.exe)?\s+//?[ck]|time|env|nice|start(?:\s+/\w+)*)\s+)*(?:[A-Za-z_][A-Za-z0-9_]*=[^\s;|&(){}]*\s+)*(?:['"][^'"\n]*[\\/]|['"]?(?:[^\s'";|&(){}]*[\\/])?)"#;
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

fn compile(programs: &[&str], extras: &[String]) -> Regex {
    let mut alternation = programs.join("|").replace('@', NAME_END);
    for name in extras {
        alternation.push('|');
        alternation.push_str(&regex::escape(name));
    }
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
        .map(|m| {
            m.as_str()
                .trim_matches(|c: char| c.is_whitespace() || ";|&(){}".contains(c))
                .to_string()
        })
}

/// Separate gated targets from commands outside the wrapper. A wrapper must itself
/// start a command; mentioning it in an echo argument cannot bless another build.
fn gate_prefix() -> Regex {
    Regex::new(&format!(
        r#"(?i){START}heavy(?:\.exe)?['"]?\s+run\s+(?:(?:--label\s+(?:'[^']*'|"[^"]*"|\S+)|--max-wait\s+\S+|--no-jobs-env)\s+)*(?:--\s+)?"#,
    )).expect("gate regex")
}

fn gated_targets(command: &str) -> (Vec<String>, String) {
    let re = gate_prefix();
    let mut targets = Vec::new();
    let mut outside = String::new();
    let mut from = 0;
    while let Some(m) = re.find_at(command, from) {
        let mut depth = 0_u32;
        let mut quote = None;
        let mut end = command.len();
        for (offset, ch) in command[m.end()..].char_indices() {
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                }
                continue;
            }
            if ch == '\'' || ch == '"' {
                quote = Some(ch);
                continue;
            }
            match ch {
                // Internal markers keep an expanded shell body within its wrapper.
                '\u{1e}' | '(' | '{' => depth += 1,
                '\u{1f}' | ')' | '}' if depth > 0 => depth -= 1,
                '\n' | ';' | '|' | '&' | ')' | '}' if depth == 0 => {
                    end = m.end() + offset;
                    break;
                }
                _ => {}
            }
        }
        outside.push_str(&command[from..m.start()]);
        outside.push_str("; ");
        targets.push(command[m.end()..end].to_string());
        from = end;
    }
    outside.push_str(&command[from..]);
    (targets, outside)
}

/// Blanks text that is data rather than commands, so it cannot look like a command start:
/// heredoc and here-string bodies, and quoted arguments. A quoted string stays visible where it
/// is the program itself (`& "C:\...\cargo.exe" build`) or the body of `-c`/`-Command`/`/c`.
/// `powershell` says which shell reads `command`: there a quoted string at a command start is a
/// program only after the call operator (`"make sure ..."` just prints), while Bash and cmd run it.
fn mask_data(command: &str, powershell: bool) -> String {
    mask_data_inner(&command.replace("\r\n", "\n"), powershell, 0)
}

fn mask_data_inner(command: &str, powershell: bool, depth: usize) -> String {
    // Unusually deep shell nesting is outside this heuristic parser's contract.
    if depth >= 32 {
        return String::new();
    }
    let heredoc =
        Regex::new(r#"<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?"#).expect("heredoc regex");
    let mut text = command.to_string();
    let mut from = 0;
    while let Some(caps) = heredoc.captures(&text[from..]) {
        let delimiter = caps[1].to_string();
        let marker_end = from + caps.get(0).unwrap().end();
        let Some(body_start) = text[marker_end..].find('\n').map(|i| marker_end + i + 1) else {
            break;
        };
        let mut end = text.len();
        let mut pos = body_start;
        for line in text[body_start..].split_inclusive('\n') {
            if line.trim() == delimiter {
                end = pos;
                break;
            }
            pos += line.len();
        }
        // Preserve byte offsets even when the data contains multibyte UTF-8.
        let blank: String = text.as_bytes()[body_start..end]
            .iter()
            .map(|&b| if b == b'\n' { '\n' } else { ' ' })
            .collect();
        text.replace_range(body_start..end, &blank);
        from = end.min(text.len());
    }

    let chars: Vec<char> = text.chars().collect();
    let gate = gate_prefix();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // PowerShell here-string: @' ... '@ or @" ... "@ with the closer at a line start.
        if c == '@'
            && i + 2 < chars.len()
            && (chars[i + 1] == '\'' || chars[i + 1] == '"')
            && chars[i + 2] == '\n'
        {
            let close: String = ['\n', chars[i + 1], '@'].iter().collect();
            let rest: String = chars[i..].iter().collect();
            let len = rest.find(&close).map_or(rest.chars().count(), |b| {
                rest[..b + close.len()].chars().count()
            });
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
            let wrapped_program = gate.find_iter(&out).any(|m| m.end() == out.len());
            let program = wrapped_program
                || if powershell {
                    before.ends_with('&') && !before.ends_with("&&")
                } else {
                    before.is_empty() || before.ends_with(|ch: char| ";|&({\n".contains(ch))
                };
            // cmd's /c and /k, spelled //c and //k in Git Bash so MSYS leaves them alone.
            let head = lower.rsplit_once(' ').map_or("", |(head, _)| head);
            let shell = head
                .split_whitespace()
                .rev()
                .find(|word| !word.starts_with('-'))
                .unwrap_or("")
                .trim_matches(['\'', '"']);
            let shell_name = shell.rsplit(['/', '\\']).next().unwrap_or(shell);
            let shell_name = shell_name.strip_suffix(".exe").unwrap_or(shell_name);
            let inner_powershell =
                ["pwsh", "pwsh.exe", "powershell", "powershell.exe"].contains(&shell_name);
            let cmd = ["cmd", "cmd.exe"].contains(&shell_name)
                && [" /c", " /k", " //c", " //k"]
                    .iter()
                    .any(|f| lower.ends_with(f));
            let script = cmd
                || ((inner_powershell
                    || ["bash", "sh", "zsh", "dash", "ksh"].contains(&shell_name))
                    && [" -c", " -lc", " -ic", " -command"]
                        .iter()
                        .any(|f| lower.ends_with(f)));
            if cmd {
                // cmd strips its outer quote pair: /c ""C:\a b\x.exe" -y".
                // Stop at the outer shell's next separator, not physical line-end:
                // a trailing `; make` must not become part of a gated cmd body.
                let mut quote = None;
                let mut line_end = chars.len();
                for (pos, &ch) in chars.iter().enumerate().skip(i) {
                    if let Some(q) = quote {
                        if ch == q {
                            quote = None;
                        }
                    } else if ch == '\'' || ch == '"' {
                        quote = Some(ch);
                    } else if ";&|\n".contains(ch) {
                        line_end = pos;
                        break;
                    }
                }
                let line: String = chars[i..line_end].iter().collect();
                let body = line.trim_end();
                let inner = body
                    .strip_prefix('"')
                    .and_then(|b| b.strip_suffix('"'))
                    .unwrap_or(body);
                out.push_str("\u{1e}; ");
                out.push_str(&mask_data_inner(inner, false, depth + 1));
                out.push_str(" ;\u{1f}");
                i = line_end;
                continue;
            }
            if script {
                // The body is itself a command line, read by the shell the flag belongs to: open it as a
                // new command.
                let inner = segment.trim_start_matches(c).trim_end_matches(c);
                out.push_str("\u{1e}; ");
                out.push_str(&mask_data_inner(inner, inner_powershell, depth + 1));
                out.push_str(" ;\u{1f}");
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

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Deny(String),
}

pub fn judge(cfg: &Config, command: &str, is_subagent: bool, powershell: bool) -> Verdict {
    if cfg.hook.disabled || command.len() > 1_048_576 {
        return Verdict::Pass;
    }
    let build_re = compile(BUILD_PROGRAMS, &cfg.hook.extra_build_programs);
    let visible = mask_data(command, powershell);
    let (targets, outside) = gated_targets(&visible);
    let build = first_run(&build_re, &outside);
    let heavy = first_run(
        &compile(HEAVY_PROGRAMS, &cfg.hook.extra_heavy_programs),
        &outside,
    );
    let wrapped_build = targets.iter().find_map(|t| first_run(&build_re, t));

    if is_subagent && cfg.subagent_builds == "deny" && (build.is_some() || wrapped_build.is_some())
    {
        let what = build.or(wrapped_build).unwrap_or_default();
        return Verdict::Deny(format!(
            "The configured policy is that subagents do not build, compile, or run test builds. \
             Blocked: `{what}`. Finish your edits and hand the exact command back to the lead, \
             including what to check in its output. The lead runs the build through the shared gate."
        ));
    }
    if build.is_none() && heavy.is_none() {
        return Verdict::Pass;
    }
    let what = build.or(heavy).unwrap_or_default();
    let heavy_path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "heavy".into());
    Verdict::Deny(format!(
        "Machine-wide memory rule: heavy commands (builds, compiles, Blender, video renders) run through the shared \
         gate so parallel sessions cannot exhaust RAM. Blocked: `{what}`. Rerun the same command with only the heavy \
         part prefixed by `heavy run -- `, for example `heavy run -- cargo build --release` or \
         `cd app && heavy run -- npm run build` (executable path if `heavy` is not on PATH: {heavy_path:?}). \
         The gate may wait when the machine is busy; `heavy status` shows slots and memory."
    ))
}

/// Reads the PreToolUse payload from stdin and prints a decision only when denying.
pub fn run(cfg: &Config) -> i32 {
    if cfg.hook.disabled {
        return 0;
    }
    let mut input = String::new();
    if std::io::stdin()
        .take(1_048_577)
        .read_to_string(&mut input)
        .is_err()
        || input.len() > 1_048_576
    {
        return 0;
    }
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&input) else {
        return 0;
    };
    let tool = payload["tool_name"].as_str().unwrap_or("");
    if tool != "Bash" && tool != "PowerShell" {
        return 0;
    }
    let Some(command) = payload["tool_input"]["command"].as_str() else {
        return 0;
    };
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
