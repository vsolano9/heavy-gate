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

/// Start of a command, plus PowerShell `&`, `cmd /c`, bash `VAR=x`, `env`/`time` prefixes and shell
/// keywords that begin a command (`then`, `do`, `if`, ...), which count only after a separator.
const START: &str = r#"(?:^|[\n;|&({`])\s*(?:&\s*)?(?:(?:then|do|else|elif|if|while|until|!|cmd(?:\.exe)?\s+//?[ck]|time|env|nice(?:\s+(?:-n\s*-?\d+|-\d+|--adjustment=-?\d+))*|timeout(?:\s+(?:-[ks]\s*\S+|--[a-z-]+(?:=\S+)?|-[a-z]+))*\s+\d+(?:\.\d+)?[smhd]?|start(?:\s+/\w+)*|[A-Za-z_][A-Za-z0-9_]*=[^\s;|&(){}]*)\s+)*(?:['"][^'"\n]*[\\/]|['"]?(?:[^\s'";|&(){}]*[\\/])?)"#;
/// End of a program name: optional Windows extension, optional closing quote, then a separator.
const END: &str = r#"(?:\.exe|\.cmd|\.bat)?['"]?(?:\s|$|[;|&)}`])"#;

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
/// Commands longer than this are not parsed; they are only searched for a build anywhere.
const MAX_CLASSIFIED: usize = 1 << 20;
/// Hook payloads longer than this are not read and pass.
const MAX_PAYLOAD: u64 = 8 << 20;
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
                .trim_matches(|c: char| c.is_whitespace() || ";|&(){}`".contains(c))
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

/// Where a wrapped target that starts at `start` ends: the first separator outside quotes and
/// nesting, or the end of the text with nesting balanced. `None` when nesting never closes.
fn target_end(command: &str, start: usize, brackets: bool, powershell: bool) -> Option<usize> {
    let mut depth = 0_u32;
    let mut quote = None;
    for (offset, ch) in command[start..].char_indices() {
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
        // Braces group commands only in PowerShell script blocks; in Bash an argument brace is
        // literal (`echo { ; make` runs make outside the wrapper).
        let opens = ch == '\u{1e}' || (brackets && (ch == '(' || (powershell && ch == '{')));
        let closes = ch == '\u{1f}' || (brackets && (ch == ')' || (powershell && ch == '}')));
        if opens {
            depth += 1;
        } else if closes && depth > 0 {
            depth -= 1;
        } else if depth == 0 && matches!(ch, '\n' | ';' | '|' | '&' | ')' | '}') {
            return Some(start + offset);
        }
    }
    (depth == 0).then_some(command.len())
}

fn gated_targets(command: &str, powershell: bool) -> (Vec<String>, String, Vec<(usize, usize)>) {
    let re = gate_prefix();
    let mut targets = Vec::new();
    let mut ranges = Vec::new();
    let mut outside = String::new();
    let mut from = 0;
    while let Some(m) = re.find_at(command, from) {
        // An unbalanced bracket would otherwise extend the target to the end of the line and
        // bless whatever follows; then only the internal markers count as nesting.
        let end = target_end(command, m.end(), true, powershell).unwrap_or_else(|| {
            target_end(command, m.end(), false, powershell).unwrap_or(command.len())
        });
        outside.push_str(&command[from..m.start()]);
        outside.push_str("; ");
        targets.push(command[m.end()..end].to_string());
        ranges.push((m.start(), end));
        from = end;
    }
    outside.push_str(&command[from..]);
    (targets, outside, ranges)
}

/// Blanks text that is data rather than commands, so it cannot look like a command start:
/// heredoc and here-string bodies, and quoted arguments. A quoted string stays visible where it
/// is the program itself (`& "C:\...\cargo.exe" build`) or the body of `-c`/`-Command`/`/c`.
/// `powershell` says which shell reads `command`: there a quoted string at a command start is a
/// program only after the call operator (`"make sure ..."` just prints), while Bash and cmd run it.
struct Masked {
    /// The command with CRLF line ends normalized.
    text: String,
    /// `text` with data blanked and nested bodies marked.
    visible: String,
    /// For each byte of `visible`, the byte of `text` it stands for.
    origin: Vec<usize>,
}

fn mask_data(command: &str, powershell: bool) -> Masked {
    let text = command.replace("\r\n", "\n");
    let (visible, origin) = mask_data_inner(&text, powershell, 0);
    Masked {
        text,
        visible,
        origin,
    }
}

/// Appends `s` to the masked text, recording that its bytes stand for source byte `from`.
fn emit(out: &mut String, origin: &mut Vec<usize>, s: &str, from: usize) {
    out.push_str(s);
    origin.resize(out.len(), from);
}

fn mask_data_inner(command: &str, powershell: bool, depth: usize) -> (String, Vec<usize>) {
    // Unusually deep shell nesting is outside this heuristic parser's contract.
    if depth >= 32 {
        return (String::new(), Vec::new());
    }
    // `<<<` is a here-string (one word of data), not a heredoc with a body.
    let heredoc = Regex::new(r#"(?:^|[^<])<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?"#)
        .expect("heredoc regex");
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
    // Byte offset of each char, plus the end.
    let at: Vec<usize> = text
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(text.len()))
        .collect();
    let gate = gate_prefix();
    let mut out = String::with_capacity(text.len());
    let mut origin = Vec::with_capacity(text.len());
    let mut buf = [0_u8; 4];
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
            emit(&mut out, &mut origin, "@''@", at[i]);
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
            let last = j.min(chars.len() - 1);
            let close = at[j.min(chars.len())];
            let segment: String = chars[i..last + 1].iter().collect();
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
                let base = at[i] + usize::from(inner.len() != body.len());
                emit(&mut out, &mut origin, "\u{1e}; ", at[i]);
                let (masked, from) = mask_data_inner(inner, false, depth + 1);
                out.push_str(&masked);
                origin.extend(from.iter().map(|o| o + base));
                emit(&mut out, &mut origin, " ;\u{1f}", at[line_end]);
                i = line_end;
                continue;
            }
            if script {
                // The body is itself a command line, read by the shell the flag belongs to: open it as a
                // new command.
                let opened = segment.trim_start_matches(c);
                let inner = opened.trim_end_matches(c);
                let base = at[i] + (segment.len() - opened.len());
                emit(&mut out, &mut origin, "\u{1e}; ", at[i]);
                let (masked, from) = mask_data_inner(inner, inner_powershell, depth + 1);
                out.push_str(&masked);
                origin.extend(from.iter().map(|o| o + base));
                emit(&mut out, &mut origin, " ;\u{1f}", close);
            } else if program {
                for k in i..last + 1 {
                    emit(&mut out, &mut origin, chars[k].encode_utf8(&mut buf), at[k]);
                }
            } else {
                emit(&mut out, &mut origin, c.encode_utf8(&mut buf), at[i]);
                emit(&mut out, &mut origin, c.encode_utf8(&mut buf), close);
            }
            i = j + 1;
            continue;
        }
        emit(&mut out, &mut origin, c.encode_utf8(&mut buf), at[i]);
        i += 1;
    }
    (out, origin)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Deny(String),
}

pub fn judge(cfg: &Config, command: &str, is_subagent: bool, powershell: bool) -> Verdict {
    if cfg.hook.disabled {
        return Verdict::Pass;
    }
    let build_re = compile(BUILD_PROGRAMS, &cfg.hook.extra_build_programs);
    if command.len() > MAX_CLASSIFIED {
        // Too large to parse: deny if a build appears anywhere rather than pass it unseen.
        return match first_run(&build_re, command) {
            Some(found) => Verdict::Deny(format!(
                "Machine-wide memory rule: this command is too large for the hook to classify and \
                 contains `{found}`. Run the heavy part as its own command, prefixed by `heavy run -- `."
            )),
            None => Verdict::Pass,
        };
    }
    let masked = mask_data(command, powershell);
    let (targets, outside, ranges) = gated_targets(&masked.visible, powershell);
    let heavy_re = compile(HEAVY_PROGRAMS, &cfg.hook.extra_heavy_programs);
    let build = first_run(&build_re, &outside);
    let heavy = first_run(&heavy_re, &outside);
    let wrapped_build = targets.iter().find_map(|t| first_run(&build_re, t));

    if is_subagent && cfg.subagent_builds == "deny" && (build.is_some() || wrapped_build.is_some())
    {
        let what = span_at(&masked, &build_re, &ranges, build.is_none())
            .unwrap_or_else(|| command_span(command, &build.or(wrapped_build).unwrap_or_default()));
        return Verdict::Deny(format!(
            "The configured policy is that subagents do not build, compile, or run test builds. \
             Blocked: `{what}`. Finish your edits and hand the exact command back to the lead, \
             including what to check in its output. The lead runs the build through the shared gate."
        ));
    }
    if build.is_none() && heavy.is_none() {
        return Verdict::Pass;
    }
    let re = if build.is_some() {
        &build_re
    } else {
        &heavy_re
    };
    let what = span_at(&masked, re, &ranges, false)
        .unwrap_or_else(|| command_span(command, &build.or(heavy).unwrap_or_default()));
    let wrapped = wrapped_form(&what);
    let heavy_path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "heavy".into());
    Verdict::Deny(format!(
        "Machine-wide memory rule: heavy commands (builds, compiles, Blender, video renders) run through the shared \
         gate so parallel sessions cannot exhaust RAM. Blocked: `{what}`. Rerun it as `{wrapped}`, leaving the rest \
         of the command line as it was (executable path if `heavy` is not on PATH: {heavy_path:?}). \
         The gate may wait when the machine is busy; `heavy status` shows slots and memory."
    ))
}

/// Shell prefixes that come before the program: control keywords, PowerShell `&`, `cmd /c`,
/// `VAR=value` (quoted values allowed), `time`, `env`, `nice`, `timeout` and `start`.
fn prefix_regex() -> Regex {
    Regex::new(
        r#"(?i)^\s*(?:&\s*)?(?:(?:then|do|else|elif|if|while|until|!|cmd(?:\.exe)?\s+//?[ck]|time|env|nice(?:\s+(?:-n\s*-?\d+|-\d+|--adjustment=-?\d+))*|timeout(?:\s+(?:-[ks]\s*\S+|--[a-z-]+(?:=\S+)?|-[a-z]+))*\s+\d+(?:\.\d+)?[smhd]?|start(?:\s+/\w+)*|[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|[^\s;|&(){}'"])*)\s+)*"#,
    )
    .expect("prefix regex")
}

/// Whether `ch` at byte `i` ends a simple command. `&` in a redirect (`2>&1`, `&>`) does not.
fn is_separator(text: &str, i: usize, ch: char) -> bool {
    match ch {
        '\n' | ';' | '|' | '(' | ')' | '{' | '}' | '`' => true,
        '&' => !text[..i].ends_with('>') && !text[i + 1..].starts_with('>'),
        _ => false,
    }
}

/// The blocked simple command with its prefixes, arguments and quoting, located by position in
/// the masked text (where data is blanked and nested bodies are marked) and cut from the
/// original through the mask's byte origins. `inside` selects a match inside an already
/// wrapped target rather than outside all of them. `None` when no such match exists.
fn span_at(masked: &Masked, re: &Regex, ranges: &[(usize, usize)], inside: bool) -> Option<String> {
    let visible = masked.visible.as_str();
    let prefix = prefix_regex();
    let keywords =
        Regex::new(r"^(?:(?:then|do|else|elif|if|while|until|!)\s+)+").expect("keyword regex");
    let lead = |c: char| c.is_whitespace() || ";|&(){}`\u{1e}\u{1f}".contains(c);
    for m in re.find_iter(visible) {
        let found = m.as_str();
        let start = m.start() + (found.len() - found.trim_start_matches(lead).len());
        let core = start + prefix.find(&visible[start..m.end()]).map_or(0, |p| p.end());
        if ranges.iter().any(|&(a, b)| (a..b).contains(&core)) != inside {
            continue;
        }
        // The exemption `first_run` makes.
        let next = visible[m.end()..]
            .split(|c: char| c.is_whitespace() || ";&|".contains(c))
            .find(|t| !t.is_empty())
            .unwrap_or("");
        if ["--version", "-V", "-vV", "--help", "-h", "/?"].contains(&next) {
            continue;
        }
        let mut end = visible.len();
        let mut i = core;
        while let Some(ch) = visible[i..].chars().next() {
            if ch == '\'' || ch == '"' {
                // A quote left in the masked text is a whole argument: skip to its partner.
                match visible[i + 1..].find(ch) {
                    Some(p) => {
                        i += p + 2;
                        continue;
                    }
                    None => {
                        end = i;
                        break;
                    }
                }
            }
            if ch == '\u{1e}' || ch == '\u{1f}' || is_separator(visible, i, ch) {
                end = i;
                break;
            }
            i += ch.len_utf8();
        }
        let from = *masked.origin.get(start)?;
        let to = masked.origin.get(end).copied().unwrap_or(masked.text.len());
        let span = masked.text.get(from..to)?.trim();
        return Some(keywords.replace(span, "").into_owned());
    }
    None
}

/// The whole simple command that the classifier matched, with its prefixes, arguments and
/// quoting, taken from the original text: the fallback when `span_at` finds no match. The match
/// comes from masked text, so it is located again here: the first occurrence in command position
/// outside quotes, otherwise the first in
/// command position inside a quoted body (`bash -c '...'`, where an already wrapped occurrence
/// is not in command position), otherwise the first anywhere. Falls back to the match itself.
fn command_span(command: &str, found: &str) -> String {
    let text = command.replace("\r\n", "\n");
    let prefix = prefix_regex();
    let core = prefix.find(found).map_or(found, |m| &found[m.end()..]);
    if core.is_empty() {
        return found.to_string();
    }
    let at_word = |i: usize| {
        text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || "'\";|&(){}`".contains(c))
    };
    let mut quote: Option<char> = None;
    let mut prev = '\0';
    let mut command_start = 0;
    // Where the current command starts inside a quoted body: after its opening quote or the
    // last separator in it.
    let mut body_start = 0;
    let mut located = None;
    let mut in_body = None;
    let mut fallback = None;
    let in_position = |leading: &str| {
        prefix
            .find(leading)
            .is_some_and(|m| m.end() == leading.len())
    };
    for (i, ch) in text.char_indices() {
        if text[i..].starts_with(core) && at_word(i) {
            if quote.is_none() && in_position(&text[command_start..i]) {
                located = Some((command_start, i));
                break;
            }
            if quote.is_some() && in_body.is_none() && in_position(&text[body_start..i]) {
                in_body = Some((body_start, i));
            }
            fallback.get_or_insert((i, i));
        }
        match quote {
            Some(q) => {
                if ch == q && !(q == '"' && (prev == '\\' || prev == '`')) {
                    quote = None;
                } else if is_separator(&text, i, ch) {
                    body_start = i + ch.len_utf8();
                }
            }
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                body_start = i + ch.len_utf8();
            }
            None if is_separator(&text, i, ch) => command_start = i + ch.len_utf8(),
            None => {}
        }
        prev = ch;
    }
    let Some((start, core_at)) = located.or(in_body).or(fallback) else {
        return found.to_string();
    };
    let mut quote = None;
    let mut end = text.len();
    for (offset, ch) in text[core_at..].char_indices() {
        let i = core_at + offset;
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        if offset < core.len() {
            continue;
        }
        if ch == '\'' || ch == '"' {
            // A quote opened inside the span is an argument; one that closes the enclosing body
            // ends the span. Look ahead for its partner on the same line.
            let partner = text[i + 1..]
                .split('\n')
                .next()
                .is_some_and(|rest| rest.contains(ch));
            if !partner {
                end = i;
                break;
            }
            quote = Some(ch);
            continue;
        }
        if is_separator(&text, i, ch) {
            end = i;
            break;
        }
    }
    // Control keywords (`then make`) are shell syntax, not part of the blocked command.
    let keywords =
        Regex::new(r"^(?:(?:then|do|else|elif|if|while|until|!)\s+)+").expect("keyword regex");
    keywords.replace(text[start..end].trim(), "").into_owned()
}

/// `heavy run --` placed after shell prefixes (`VAR=x`, `time`, `cmd /c`, ...) and with an
/// interpreter in front of scripts that cannot be started directly.
fn wrapped_form(span: &str) -> String {
    let cut = prefix_regex().find(span).map_or(0, |m| m.end());
    let head = span[..cut]
        .trim_start()
        .trim_start_matches('&')
        .trim_start();
    let rest = &span[cut..];
    // A quoted program path may contain spaces.
    let program = match rest.chars().next() {
        Some(q @ ('\'' | '"')) => rest[1..].split(q).next().unwrap_or(""),
        _ => rest.split_whitespace().next().unwrap_or(""),
    };
    let lower = program.to_ascii_lowercase();
    let interpreter = if lower.ends_with(".ps1") {
        "pwsh -NoProfile -File "
    } else if lower.ends_with(".sh") {
        "sh "
    } else if lower.ends_with(".py") {
        "python "
    } else {
        ""
    };
    format!("{head}heavy run -- {interpreter}{rest}")
}

/// Reads the PreToolUse payload from stdin and prints a decision only when denying.
pub fn run(cfg: &Config) -> i32 {
    if cfg.hook.disabled {
        return 0;
    }
    let mut input = String::new();
    if std::io::stdin()
        .take(MAX_PAYLOAD + 1)
        .read_to_string(&mut input)
        .is_err()
        || input.len() as u64 > MAX_PAYLOAD
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
