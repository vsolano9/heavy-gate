# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [1.0.0] - 2026-10-15

First public version, developed from a gate previously used internally to coordinate builds and renders across independent agent sessions.

### Added

- Shared command admission on Windows, Linux, and macOS, combining a slot limit with an available-memory floor.
- RAM and CPU based defaults, user configuration, effective configuration output, and non-overwriting initialization.
- `heavy run`, `status`, `config`, `init`, `check`, `hook`, `--help`, and `--version` commands, with `help` and `version` aliases.
- Cargo and CMake parallelism hints that preserve existing environment overrides.
- Windows Job Object containment attempts, Unix signal forwarding, and Linux direct-child parent-death signaling.
- Local holder metadata and JSON Lines run records.
- Optional Claude Code Bash and PowerShell hook, configurable additional programs, and subagent build policy.
- Table-driven shell-classification contracts, TOML validation, and auto-sizing arithmetic checks.
- Per-command wrapper recognition, including compounds that mix gated and ungated commands.
- Installation, integration, contribution, and security documentation under the MIT license.
