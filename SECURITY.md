# Security policy

## Scope

`heavy-gate` is a local command wrapper. It has no server, network listener, telemetry, update checker, or runtime network requests. Commands launched through it retain the user's permissions and can themselves access the network or other resources.

The gate coordinates cooperating callers. It is not a sandbox, memory quota, access-control boundary, or protection against hostile processes. The memory floor is checked at admission; running commands can subsequently exhaust memory. The Claude Code hook is heuristic and deliberately fails open on unusable hook input; configuration errors fall back to defaults for the affected keys.

On Windows a command runs only inside a kill-on-close Job Object; if the host prevents that, the command is refused. Unix processes can outlive the wrapper; Linux parent-death signaling covers the direct child. Use operating-system resource limits and isolation when stronger guarantees are needed.

## Local data

Configuration, holder metadata, Unix lock files, and `runs.jsonl` are stored in user directories described in the README. Labels, working directories, process identifiers, timing, and exit codes may reveal local project details. The default label is derived from the command. Avoid secrets in command-line arguments and labels, keep state directories private to the intended user, and redact local paths and labels before sharing logs.

The program does not upload these files. Logs have no automatic rotation. On Unix, newly created state/configuration directories use mode 0700 and files use mode 0600, subject to the user's umask; existing permissions are preserved. Do not place the state directory in an untrusted shared location, and do not delete or replace active Unix lock files. An untrusted configuration or executable on `PATH` can change behavior; treat both as part of the local trust boundary.

Checksums detect differences from a manifest. A manifest downloaded from the same release is not an independent signature of publisher identity.

## Report a vulnerability

Report sensitive findings privately through [GitHub's private vulnerability reporting form](https://github.com/vsolano9/heavy-gate/security/advisories/new). Include the version or commit, affected OS, reproduction steps, expected and observed behavior, and impact. Remove credentials and private local data from the report.

Use public issues for ordinary bugs that do not expose sensitive information. If private reporting is unavailable, open a public issue requesting a private reporting channel without including exploit details or private data. No response-time or remediation deadline is promised. Security fixes target the latest maintained release; this repository does not promise backports to earlier releases.
