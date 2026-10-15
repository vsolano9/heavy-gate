# Contributing

Keep changes focused on dependable command admission across independent sessions. The public interface consists of the CLI, configuration keys, hook payload handling, exit codes, and local record formats documented in [README.md](README.md).

## Set up

Use Rust 1.85 or later and the native linker for your platform. Clone the repository and inspect the code before running it. The binary is named `heavy`; the Cargo package is `heavy-gate`.

```sh
git clone https://github.com/vsolano9/heavy-gate.git
cd heavy-gate
```

If you already have `heavy`, route builds through it:

```sh
heavy run -- cargo build
```

To bootstrap a machine without an installed wrapper, run `cargo build` once with other heavy work stopped, then use `target/debug/heavy` (`target\debug\heavy.exe` on Windows) to gate subsequent builds. Keep one build coordinator per machine; implementation workers should return exact build commands to their lead rather than building concurrently.

## Verify a change

Run the existing suite once on the candidate and exercise the affected real CLI path. The commands below should pass on Windows, Linux, and macOS; report which platforms you actually exercised.

```sh
cargo fmt --check
heavy run -- cargo clippy --all-targets -- -D warnings
heavy run -- cargo test
```

Use an isolated temporary configuration and state directory for manual concurrency checks so existing users' runs are unaffected. Do not delete active lock files.

For gate changes, check slot contention, memory-floor waiting, timeout without child start, child start failure, exit-code propagation, environment preservation, and release after normal and interrupted exits. Check platform lifecycle behavior on the relevant OS rather than inferring it from another OS's build. On Windows, a debug build run with `HEAVY_GATE_TEST_JOB_FALLBACK=1` takes the suspended-start path used when the host forbids nested jobs; release builds ignore the variable.

For configuration changes, inspect `heavy config`, create a fresh file with `heavy init`, verify overwrite refusal, and check invalid keys and values. Hook configuration errors must stay silent and fall back to defaults for the affected keys, as `run` does.

For hook changes, add a focused case to the existing hook cases for each fixed false positive or false negative. Include Bash and PowerShell quoting where relevant, wrapped commands, subagent behavior, and commands that must pass as data. Also exercise the real entry points:

```sh
heavy check 'cargo build'
heavy check 'heavy run -- cargo build'
heavy check --subagent 'heavy run -- cargo build'
printf '%s\n' '{"tool_name":"Bash","tool_input":{"command":"cargo build"}}' | heavy hook
```

The first check should deny with defaults, the second should pass, the third should deny, and the hook should emit a JSON denial. None of these commands runs the build being checked. Include malformed payload checks so the hook cannot turn an unrelated tool call into a blocking error.

## Review and submit

- Explain the concrete problem, resulting behavior, and compatibility implications.
- Update the README and example configuration for interface changes, and add a user-facing changelog entry under `Unreleased`.
- Include exact checks and observed results, platform, and remaining coverage gaps. Never describe an unrun platform check as passed.
- Keep generated output, local configurations, logs, private paths, and credentials out of commits.
- Preserve the MIT license and check licenses before adding dependencies or borrowed code.

Open a pull request against [vsolano9/heavy-gate](https://github.com/vsolano9/heavy-gate). There is no GitHub Actions workflow; validation and release preparation run locally. Do not add CI workflows as part of an unrelated contribution.

## Release builds

Build each release target with its supported native toolchain. Targets are `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`, and `aarch64-apple-darwin`. Gate each compile with `heavy run -- cargo build --release --target TARGET`.

When dependencies change, have the build coordinator run `heavy run -- cargo generate-lockfile`, inspect and commit the updated `Cargo.lock`, then use `--locked` on Clippy, test, and release commands. Validate on all three native platforms; a cross-compile does not exercise process cleanup or memory APIs.

Use Rust's `--remap-path-prefix` option to keep local source paths out of release artifacts. Merge this with any existing `RUSTFLAGS` rather than discarding required flags. For a checkout path without whitespace, examples are:

```sh
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--remap-path-prefix=$(pwd)=/src/heavy-gate" heavy run -- cargo build --release --target x86_64-unknown-linux-gnu
```

```powershell
$prefix = (Get-Location).Path
$env:RUSTFLAGS = (($env:RUSTFLAGS + ' --remap-path-prefix=' + $prefix + '=/src/heavy-gate').Trim())
heavy run -- cargo build --release --target x86_64-pc-windows-msvc
```

When the path contains whitespace, use Cargo's `CARGO_ENCODED_RUSTFLAGS` with the unit separator between complete flag arguments instead of splitting the remap argument on spaces. Remapping the checkout does not cover paths from every dependency or toolchain; inspect the resulting binaries before distribution.

Verify version output and representative CLI behavior from the packaged executable on each target OS. Name archives as listed in the README, compute `SHA256SUMS` from the finished archives, and verify the manifest against those same files. Creating archives and checksums is separate from publishing them; do not claim a release is available until its uploaded assets have been checked.
