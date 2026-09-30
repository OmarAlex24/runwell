# runwell-setup

Read-only Linux host discovery through the system `ssh` binary, plus an
interactive or agent-driven setup skeleton. No SSH library or password prompt is
used. Batch authentication is mandatory. Key enrollment commands are printed for
the user to run in their own terminal.

```sh
runwell setup
runwell setup --host builder@example.invalid --repo example/project --json
runwell setup probe --host builder@example.invalid:2222 --json
```

Host probes run the embedded POSIX script through `ssh … sh -s`. The script
reads host metadata and prints one JSON document. It does not install tools,
create temporary files, execute runner binaries, or read runner credentials.
Missing utilities, unsupported platforms, and access restrictions become explicit
unknown reasons. `sudo -n true` checks privileged access; fixed read-only queries
may then use `sudo -n`. The caller has a 120-second deadline and kills timed-out
SSH processes.

Each fact has `value` and `unknown_reason`. Absent or null fields from older or
partial JSON documents normalize to unknown. Malformed or truncated documents
fail with a typed error. Sizes use bytes, PSI uses percentages, and sar
percentiles use aggregate CPU utilization (`100 - %idle`). The sar sample covers
the previous seven complete calendar days, includes only readable archives, and
uses nearest-rank p50/p95. Missing daily archives are omitted; no usable archives
produce an unknown observation. Labels are unknown when `.runner` does not
record them. Runner publication ages are enriched locally from public GitHub
release metadata with a five-second request timeout; offline or rate-limited
lookups stay unknown. No host identity or input repository is sent to that API.
Warnings are written to stderr for JSON commands, leaving stdout parseable.
The service-container warning requires workload traces, which this skeleton
exposes through the recommender input but does not fetch yet.

The wizard resumes `runwell/setup-session.json` under the platform configuration
directory (`~/Library/Application Support` on macOS, `$XDG_CONFIG_HOME` or
`~/.config` on Linux). `--state-file PATH` overrides it. Saves use a same-directory
atomic rename and mode 0600 on Unix; discovery progress is saved after each host.
`setup probe` neither loads nor saves a session. Explicit `--host`, `--repo`, or
`--json` runs have no prompts and report key-auth failures immediately.

`Recommender` consumes host facts and repository traces, returning architectures
ranked with predicted p50/p90 durations. The current stub returns
`Error::Unimplemented`; discovery still saves the session with a null
recommendation and plan. `Applier` separates explicit install-step planning from
execution. The dry-run implementation requires the exact text `APPLY` and only
prints the plan. This milestone cannot select or install an architecture.

Tests use JSON fixtures, local command shims for Linux runner/sar behavior, and a
fake `ssh` on an isolated subprocess PATH for success, auth failure, and timeout.
They do not connect to real hosts or GitHub. Run `shellcheck src/probe.sh` and
`sh -n src/probe.sh` alongside the workspace Rust checks.
