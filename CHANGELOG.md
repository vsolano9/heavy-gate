# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [1.0.1] - 2026-10-15

### Fixed

- The hook now recognizes a quoted build subcommand or script path (`cargo "build"`, `pwsh -File './build.ps1'`), which the shell passes on as the plain word.
- The hook recognizes global options before a build subcommand (`cargo --offline build`, `npm --prefix web run build`, `go -C sub build`).
- PowerShell strings end where PowerShell ends them: a backslash before the closing quote (`"C:\"`) no longer hides the rest of the command.
- Command substitutions run where the shell runs them: `$(...)` and Bash backticks inside double quotes are checked as commands, and a substitution in the arguments of `heavy run --` counts as outside the wrapper, since it runs before `heavy` starts.
- `heavy run` passes arguments that are not valid UTF-8 (such as non-UTF-8 file names on Unix) through to the command instead of panicking.

## [1.0.0] - 2026-10-15

First public version, developed from a gate previously used internally to coordinate builds and renders across independent agent sessions.

### Added

- Shared command admission on Windows, Linux, and macOS, combining a slot limit with an available-memory floor.
- RAM and CPU based defaults, user configuration, effective configuration output, and non-overwriting initialization.
- `heavy run`, `status`, `config`, `init`, `check`, `hook`, `--help`, and `--version` commands, with `help` and `version` aliases.
- Cargo and CMake parallelism hints that preserve existing environment overrides.
- Windows Job Object containment (the command is refused when the wrapper cannot join the job) and Ctrl+C handling, Unix signal forwarding, and Linux direct-child parent-death signaling.
- Local holder metadata and JSON Lines run records.
- Optional Claude Code Bash and PowerShell hook, configurable additional programs, and subagent build policy.
- Table-driven shell-classification contracts, TOML validation, and auto-sizing arithmetic checks.
- Per-command wrapper recognition, including compounds that mix gated and ungated commands.
- Installation, integration, contribution, and security documentation under the MIT license.
