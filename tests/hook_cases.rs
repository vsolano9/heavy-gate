//! Classification contracts. These strings are input data, never executed commands.

use heavy_gate::config::LoadedConfig;
use heavy_gate::hook::{judge, Verdict};

#[test]
fn shell_command_cases() {
    // (description, command, PowerShell, subagent, deny)
    let cases = [
        (
            "unicode heredoc",
            "cat <<EOF\n😀 cargo build\nEOF\necho café",
            false,
            false,
            false,
        ),
        (
            "unicode heredoc then build",
            "cat <<EOF\n日本語 cargo build\nEOF\nmake",
            false,
            false,
            true,
        ),
        (
            "bash exe body",
            "bash.exe -c 'cargo build'",
            false,
            false,
            true,
        ),
        (
            "bash exe wrapped subagent",
            "heavy run -- bash.exe -c 'cargo build'",
            false,
            true,
            true,
        ),
        (
            "ps bash exe path",
            r#"& 'C:\Program Files\Git\bin\bash.exe' -c 'cargo build'"#,
            true,
            false,
            true,
        ),
        (
            "path wrapper after build",
            "make;./heavy run -- cargo build",
            false,
            false,
            true,
        ),
        (
            "path wrapper after and",
            "make&&./heavy run -- true",
            false,
            false,
            true,
        ),
        (
            "quoted target subagent",
            "heavy run -- '/usr/bin/make' all",
            false,
            true,
            true,
        ),
        (
            "ps quoted target subagent",
            r#"heavy run -- 'C:\Tools\cargo.exe' build"#,
            true,
            true,
            true,
        ),
        (
            "cmd then outer build",
            r#"heavy run -- cmd /c "cargo build"; make"#,
            true,
            false,
            true,
        ),
        (
            "cmd then bash build",
            r#"heavy run -- cmd /c "cargo build" && make"#,
            false,
            false,
            true,
        ),
        ("cargo", "cargo build --release", false, false, true),
        ("cargo alias", "cargo b", false, false, true),
        ("toolchain", "cargo +stable check", false, false, true),
        ("compiler", "rustc main.rs", false, false, true),
        ("cpp compiler", "clang++ main.cpp", false, false, true),
        ("dotnet", "dotnet publish", false, false, true),
        ("cmake", "cmake --build out", false, false, true),
        ("npm", "npm run build", false, false, true),
        ("pnpm", "pnpm typecheck", false, false, true),
        ("yarn", "yarn build", false, false, true),
        ("npx", "npx vite build", false, false, true),
        ("go", "go test ./...", false, false, true),
        ("swift", "swift build", false, false, true),
        ("zig", "zig build", false, false, true),
        ("gradle", "./gradlew assemble", false, false, true),
        (
            "build script",
            "./scripts/build-release.sh",
            false,
            false,
            true,
        ),
        (
            "interpreted script",
            "python scripts/build.py",
            false,
            false,
            true,
        ),
        ("unix path", "/usr/bin/make all", false, false, true),
        (
            "unix quoted path",
            "'/opt/build tools/make' all",
            false,
            false,
            true,
        ),
        ("ps plain build", "cargo build", true, false, true),
        (
            "ps call operator",
            r#"& "C:\Tools\cargo.exe" build"#,
            true,
            false,
            true,
        ),
        (
            "ps quoted expression",
            r#""make sure this is data""#,
            true,
            false,
            false,
        ),
        (
            "ps quoted compiler expression",
            r#""cargo build""#,
            true,
            false,
            false,
        ),
        ("ps build script", "& ./build.ps1", true, false, true),
        ("compound", "cd app && npm run build", false, false, true),
        (
            "ps compound",
            "Set-Location app; npm run build",
            true,
            false,
            true,
        ),
        ("env", "env RUSTFLAGS=-g cargo build", false, false, true),
        ("assignment", "CC=clang make", false, false, true),
        ("time", "time cargo build", false, false, true),
        ("wrapped", "heavy run -- cargo build", false, false, false),
        (
            "wrapped options",
            "heavy run --label release --max-wait 10 --no-jobs-env -- cargo build",
            false,
            false,
            false,
        ),
        (
            "wrapped legacy syntax",
            "heavy run cargo build",
            false,
            false,
            false,
        ),
        (
            "wrapped compound",
            "cd app && heavy run -- npm run build",
            false,
            false,
            false,
        ),
        (
            "ps wrapped path",
            r#"& "C:\Tools\heavy.exe" run -- cargo build"#,
            true,
            false,
            false,
        ),
        (
            "wrapped shell",
            "heavy run -- bash -c 'cargo build && make'",
            false,
            false,
            false,
        ),
        (
            "wrapped pwsh",
            "heavy run -- pwsh -NoProfile -Command 'cargo build; make'",
            true,
            false,
            false,
        ),
        (
            "mixed wrappers",
            "heavy run -- cargo build && npm run build",
            false,
            false,
            true,
        ),
        (
            "build before wrapper",
            "make && heavy run -- cargo build",
            false,
            false,
            true,
        ),
        (
            "two wrappers",
            "heavy run -- cargo build && heavy run -- npm run build",
            false,
            false,
            false,
        ),
        (
            "wrapper text",
            "echo 'heavy run -- cargo build'; make",
            false,
            false,
            true,
        ),
        (
            "unquoted wrapper text",
            "echo heavy run -- cargo build; make",
            false,
            false,
            true,
        ),
        (
            "commit message",
            "git commit -m 'fix; cargo build; make'",
            false,
            false,
            false,
        ),
        (
            "grep string",
            "rg 'cargo build; make' src",
            false,
            false,
            false,
        ),
        (
            "echo string",
            "echo 'hello; cargo build'",
            false,
            false,
            false,
        ),
        (
            "ps data",
            "Write-Output 'hello; cargo build'",
            true,
            false,
            false,
        ),
        (
            "non-shell flag",
            "echo -c 'cargo build'",
            false,
            false,
            false,
        ),
        (
            "heredoc",
            "cat <<'EOF'\ncargo build\nmake\nEOF\n",
            false,
            false,
            false,
        ),
        (
            "tab heredoc",
            "cat <<-EOF\n\tcargo build\n\tEOF\n",
            false,
            false,
            false,
        ),
        (
            "after heredoc",
            "cat <<EOF\ncargo build\nEOF\nmake",
            false,
            false,
            true,
        ),
        (
            "ps here string",
            "$text = @'\ncargo build\n'@\nWrite-Output $text",
            true,
            false,
            false,
        ),
        (
            "ps double here string",
            "$text = @\"\nmake\n\"@\nWrite-Output $text",
            true,
            false,
            false,
        ),
        (
            "ps CRLF here string",
            "$text = @'\r\ncargo build\r\n'@\r\nWrite-Output $text",
            true,
            false,
            false,
        ),
        (
            "after ps here string",
            "$text = @'\ncargo build\n'@\nmake",
            true,
            false,
            true,
        ),
        (
            "bash body",
            "bash -c 'cd app && cargo build'",
            false,
            false,
            true,
        ),
        ("bash login body", "bash -lc 'make'", false, false, true),
        (
            "pwsh body",
            "pwsh -Command 'cargo build'",
            false,
            false,
            true,
        ),
        (
            "pwsh flags body",
            "pwsh -NoProfile -Command 'cargo build'",
            true,
            false,
            true,
        ),
        (
            "pwsh body expression",
            "pwsh -Command '\"make sure\"'",
            false,
            false,
            false,
        ),
        ("cmd plain body", "cmd /c cargo build", false, false, true),
        ("cmd body", "cmd /c \"cargo build\"", false, false, true),
        (
            "cmd quote stripping",
            r#"cmd /c ""C:\Program Files\Tools\cargo.exe" build""#,
            true,
            false,
            true,
        ),
        (
            "cmd MSYS",
            r#"cmd //c ""C:\Program Files\Tools\cargo.exe" build""#,
            false,
            false,
            true,
        ),
        (
            "cmd wrapped",
            r#"cmd /c "heavy run -- cargo build""#,
            true,
            false,
            false,
        ),
        ("version", "rustc --version", false, false, false),
        ("make version", "make --version", false, false, false),
        ("cargo version", "cargo --version", false, false, false),
        ("help", "blender --help", false, false, false),
        (
            "version then build",
            "make --version && make all",
            false,
            false,
            true,
        ),
        (
            "blender",
            "blender --background scene.blend",
            false,
            false,
            true,
        ),
        (
            "remotion",
            "remotion render src/index.ts",
            false,
            false,
            true,
        ),
        ("npx remotion", "npx remotion render", false, false, true),
        ("hyperframes", "hyperframes render", false, false, true),
        (
            "ffmpeg excluded",
            "ffmpeg -i input.mp4 output.mp4",
            false,
            false,
            false,
        ),
        ("subagent build", "cargo build", false, true, true),
        (
            "subagent wrapped build",
            "heavy run -- cargo build",
            false,
            true,
            true,
        ),
        (
            "subagent wrapped shell",
            "heavy run -- bash -c 'cargo build'",
            false,
            true,
            true,
        ),
        ("subagent read", "git status --short", false, true, false),
        ("subagent probe", "make --version", false, true, false),
        (
            "subagent render",
            "heavy run -- blender --background scene.blend",
            false,
            true,
            false,
        ),
        (
            "unbalanced brace in wrapped target",
            "heavy run -- echo { ; cargo build",
            false,
            false,
            true,
        ),
        (
            "here-string then build",
            "wc -l <<< hello
cargo build",
            false,
            false,
            true,
        ),
        (
            "here-string data",
            "grep -c x <<< 'cargo build'",
            false,
            false,
            false,
        ),
        (
            "balanced group in wrapped target",
            "heavy run -- bash -c 'cd app && { cargo build; }'",
            false,
            false,
            false,
        ),
        (
            "literal braces around unwrapped build",
            "heavy run -- echo { ; make all; echo }",
            false,
            false,
            true,
        ),
        ("do as echo data", "echo do make all", false, false, false),
        (
            "then as echo data",
            "echo then make all",
            false,
            false,
            false,
        ),
        (
            "if body build",
            "if true; then make all; fi",
            false,
            false,
            true,
        ),
        (
            "env assignment before time",
            "X=1 time cargo build -j2",
            false,
            false,
            true,
        ),
        (
            "timeout prefix",
            "timeout 600 cargo build",
            false,
            false,
            true,
        ),
        (
            "timeout options",
            "timeout -k 5 10m make -j8",
            false,
            false,
            true,
        ),
        ("nice with niceness", "nice -n 5 make", false, false, true),
        ("backtick substitution", "echo `make`", false, false, true),
        (
            "timeout words as data",
            "echo \"timeout 5 make\"",
            false,
            false,
            false,
        ),
        (
            "cmd timeout is not a prefix",
            "timeout /t 5",
            false,
            false,
            false,
        ),
        (
            "loop body build",
            "for f in a; do make; done",
            false,
            false,
            true,
        ),
        (
            "if condition build",
            "if cargo build; then echo ok; fi",
            false,
            false,
            true,
        ),
        (
            "powershell script block in wrapped body",
            "heavy run -- pwsh -NoProfile -Command { cargo build; cargo test }",
            true,
            false,
            false,
        ),
    ];
    let cfg = LoadedConfig::parse("unused.toml".into(), "").config;
    for (name, command, powershell, subagent, deny) in cases {
        let verdict = judge(&cfg, command, subagent, powershell);
        assert_eq!(
            matches!(verdict, Verdict::Deny(_)),
            deny,
            "{name}: {command:?}: {verdict:?}"
        );
    }
}

#[test]
fn configurable_policy_and_literal_program_names() {
    let loaded = LoadedConfig::parse(
        "unused.toml".into(),
        r#"
subagent_builds = 'gate'
[hook]
extra_build_programs = ['builder++', 'custom-build']
extra_heavy_programs = ['render-tool']
"#,
    );
    assert!(loaded.problems.is_empty());
    for (command, subagent, deny) in [
        ("heavy run -- cargo build", true, false),
        ("cargo build", true, true),
        ("builder++ project", false, true),
        ("builderrrrr project", false, false),
        ("/usr/bin/custom-build project", true, true),
        ("heavy run -- custom-build project", true, false),
        ("render-tool scene", false, true),
        ("heavy run -- render-tool scene", true, false),
        ("echo 'custom-build; render-tool'", true, false),
    ] {
        let verdict = judge(&loaded.config, command, subagent, false);
        assert_eq!(
            matches!(verdict, Verdict::Deny(_)),
            deny,
            "{command}: {verdict:?}"
        );
    }
    let mut cfg = loaded.config;
    cfg.subagent_builds = "deny".into();
    assert!(matches!(
        judge(&cfg, "heavy run -- builder++ project", true, false),
        Verdict::Deny(_)
    ));
    cfg.hook.disabled = true;
    assert_eq!(judge(&cfg, "cargo build", true, false), Verdict::Pass);
}

#[test]
fn deny_names_the_exact_wrapped_command() {
    let cfg = LoadedConfig::parse("unused.toml".into(), "").config;
    for (command, powershell, wrapped) in [
        (
            "cmake --build build --target release --parallel 4",
            false,
            "heavy run -- cmake --build build --target release --parallel 4",
        ),
        (
            "cd app && npm run build -- --mode production",
            false,
            "heavy run -- npm run build -- --mode production",
        ),
        (
            "FOO=1 cargo build --release",
            false,
            "FOO=1 heavy run -- cargo build --release",
        ),
        (
            "& ./build.ps1 -Release",
            true,
            "heavy run -- pwsh -NoProfile -File ./build.ps1 -Release",
        ),
        (
            "bash -c 'cargo build --locked'",
            false,
            "heavy run -- cargo build --locked",
        ),
        ("if true; then make all; fi", false, "heavy run -- make all"),
        (
            "X=1 time cargo build -j2",
            false,
            "X=1 time heavy run -- cargo build -j2",
        ),
        (
            "git commit -m \"fix: make it ok\" && make -j8",
            false,
            "heavy run -- make -j8",
        ),
        (
            "echo \"cargo build\" && cargo build --release",
            false,
            "heavy run -- cargo build --release",
        ),
        (
            "CFLAGS=\"-O2 -g\" make -j8",
            false,
            "CFLAGS=\"-O2 -g\" heavy run -- make -j8",
        ),
        (
            "cargo build 2>&1 | tee log",
            false,
            "heavy run -- cargo build 2>&1",
        ),
        (
            "& \"D:\\My Tools\\build.ps1\" -Release",
            true,
            "heavy run -- pwsh -NoProfile -File \"D:\\My Tools\\build.ps1\" -Release",
        ),
        (
            "\"./my dir/build.sh\" --fast",
            false,
            "heavy run -- sh \"./my dir/build.sh\" --fast",
        ),
        (
            "timeout 600 cargo build --release",
            false,
            "timeout 600 heavy run -- cargo build --release",
        ),
        ("echo `make -j2`", false, "heavy run -- make -j2"),
    ] {
        let Verdict::Deny(reason) = judge(&cfg, command, false, powershell) else {
            panic!("{command}: expected a denial");
        };
        assert!(
            reason.contains(&format!("`{wrapped}`")),
            "{command}: {reason}"
        );
    }
}
