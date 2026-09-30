# Standalone node (M3)

Run on an owned Linux x86-64 host with systemd as PID 1 and unified cgroup v2:

```sh
sudo runwell node --config /etc/runwell/runwell.toml --standalone
```

Start from `examples/runwell.toml`. Provision an unprivileged local runner account
and the upstream runner's OS dependencies first. Keep that account out of the
Docker group. Set `node.cpu_slots` and `node.memory_bytes` to the host capacity
available **after** OS/daemon headroom; these are explicit host capacity overrides.
Set an aggregate `standalone.ci.memory_max_bytes` that also leaves host headroom.
Startup requires at least 5% of physical RAM (minimum 256 MiB) outside that hard ceiling.
Templates and installations must be on the same filesystem for hardlinks.

M3 runs ordinary process jobs. Docker jobs/services await the M4 proxy; direct
Docker/containerd sockets are inaccessible to runner services. mTLS controller
transport is M5. The optional `[transport]` section is unused in standalone mode.
Authentication uses file references (`app`, `pat`) or environment-variable names
(`app_env`, `pat_env`); credential values are never accepted inline.

The daemon holds both a host-wide lock and a state-directory lock. The database
is root-owned, mode 0600, with WAL and FULL synchronization. The state directory
is root-owned and traversable by runners so they can reach their individual
0700 installations. Runner names depend on the stable `node.id` and SQLite's
monotonic local ID. Preserve the node identity, journal, and directory settings
across restarts.

## Public interfaces

- `runwell-admission`: existing `Resources`, `ReservationAdmission`,
  `AdmissionPolicy`, and `Brake` remain compatible. `HostAdmission` adds a
  reservation ledger, recovery, and `max_jobs`. `PsiBrake` consumes pure
  `Pressure` snapshots and monotonic milliseconds. CPU, memory, and I/O have
  independent thresholds. Its hysteresis state has a minimum dwell; high/invalid
  pressure and memory-full impose an immediate, latched admission veto. Recovery
  requires sustained low pressure. Integer scaling avoids rounding capacity up.
- `runwell-config`: validated `Config`, optional `StandaloneConfig`, class labels
  and task limits, per-resource PSI thresholds, runner release pin, host budgets,
  directories, App/PAT references, and drain/reconcile/idle/stop timeouts. Parse
  failures omit the TOML source to avoid echoing rejected inline secrets.
- `runwell-store`: `Store` is the asynchronous single-writer journal. `NewJob`,
  `Job`, `Execution`, `Runner`, `State`, `JobMeasurement`, and `PsiMeasurement` carry durable
  metadata without credentials. Focused mutations enforce legal transitions,
  unique runner identities, registration-before-launch, and deletion-before-cleanup.
- `runwell-runner`: `ReleaseClient`, `checksum`, `release_status`, `Template`,
  `clone_install`, `remove_install`, `LaunchSpec`, `classify_exit`, and
  `unregister`. Linux exposes `RunnerUser` and `stage_template`. Downloads are
  bounded, SHA-256-verified before extraction, extracted through `runuser`, sealed
  root-owned, and promoted atomically. Only immutable `bin/` and `externals/`
  files are hardlinked; writable files and `HOME` are private.
- `runwell-node`: `Controller`, `NodeBackend`, `RunnerApi`, `GithubGateway`,
  `JobPlan`, `SliceSpec`, `ProcessState`, and `run_loop`. `fake::FakeBackend`
  implements the same host port on macOS. `linux::LinuxBackend` and
  `linux::standalone` provide production wiring. `cgroup` exports pure parsers.
- `runwell node`: `--config PATH --standalone`; non-Linux execution and non-root
  Linux execution fail before opening sessions or creating host resources.

## State and durability

```text
queued -> admitted -> runner_created -> running -> completed | failed | orphaned
```

Any unfinished state may also become `failed` or `orphaned` when preparation,
acquisition, or recovery proves execution cannot continue. Terminal states cannot
be reopened. Cancellation before a runner exists is recorded as `orphaned` with
the authoritative outcome. Successful workflow completion requires a GitHub
completion event; listener exit zero alone is not workflow success.

`admitted` is persisted before `acquirejobs`. The unique runner name, directory,
unit, and template are persisted before the JIT POST; the returned agent ID and
`runner_created` transition are atomic and precede startup. JIT data is a
`secrecy::SecretString` passed only to the service environment, via a root-only
0600 environment file under the 0700 `/run/runwell` directory. Systemd's unit text
and D-Bus properties contain only its path. Cleanup removes that file after the
service is gone; credentials never enter SQLite or the persistent state directory.
The CLI logs only runwell modules at info level, excluding dependency wire traces.

GitHub may assign a different request to a matching runner. `actual_request_id`
and actual job/run/repository metadata are bound from `JobStarted`/`JobCompleted`,
using runner name/agent ID. Missing fields in later events retain known metadata. Absolute
assigned-job statistics recover demand missing from truncated event arrays;
synthetic demand still requires host admission but is already acquired upstream.
Unadmitted request IDs remain queued locally for reconsideration on messages or
the timer, with no GitHub runner registration.

Every complete batch is processed before remote ack, and only successful acks
are journaled. Handler failure retains the batch for timer retry. Repeated request
IDs and registration intents reuse durable identities. An ambiguous JIT POST is
recovered by name; `AgentExists` recovery remains in `runwell-scaleset` unchanged.

Actual slice names are `ci-rw-j<ID>.slice`: systemd interprets dash ancestry as
`ci.slice/ci-rw.slice/ci-rw-j<ID>.slice`. Each `rw-j<ID>.service` explicitly names
that slice. `ci.slice` has a persistent unit and persistent resource properties;
job slices are transient. `RemainAfterExit` retains successful service state for
polling. Slices remain available for final counters until explicit cleanup.

The listener exit status is persisted first. Remote DELETE is always attempted,
including after exit zero. After DELETE permits cleanup, remaining descendants
are stopped while the slice survives, and final counters are committed once
before slice teardown. `JobStillRunning` retains the entire cleanup
record and reservation for retry. Remote removal and local cleanup have separate
durable markers. Idle runners and listeners stuck after completion use DELETE
first, then graceful service stop, then final measurement. A completion event may
arrive up to 120 seconds after process exit before an unknown result becomes
`orphaned`. OOM is a measurement infrastructure signal; workflows are not retried
based solely on an exit code.

Startup re-adopts live registered units and restores their original reservations.
It looks up ambiguous registrations by persisted name, removes stopped/orphaned
registrations, and reconciles both unit and directory inventories. A busy unknown
registration blocks admission. Parent MemoryMax cannot be reduced while job
units exist. First SIGTERM stops admission and drains; expiry or a second signal
closes sessions while leaving live units for the next daemon to re-adopt.

The release manager checks daily, warns at 21 days behind the oldest newer
release, refreshes at 25, and fails closed if it cannot refresh by 30. An outdated
listener exit requests immediate refresh and pauses creation until the version
changes. Promotion changes only new installations. Old generations are retained
so running jobs and recovery can still reference them; M3 does not garbage-collect
template generations or completed journal history.

## Verification

Portable tests cover admission/property bounds, PSI dwell and hard vetoes,
checksum association, release deadlines, hardlinks/private homes, SQL transition
rules and reopen, and wiremock-backed registration/run/cleanup/restart/drain.

`tests/linux_privileged.rs` contains ignored, Linux-gated checks for real transient
units, cgroup counters, memory OOM enforcement, orphan reconciliation, and root
requirements. CI's `linux-privileged` job runs:

```sh
sudo -E env "PATH=$PATH" cargo test -p runwell-node -- --ignored
```

These tests require systemd PID 1, root, cgroup v2 with PSI, and `/usr/bin/python3`.
They do not download the runner or require live GitHub credentials. A real
GitHub workflow is still required to validate the full upstream JIT execution
and release archive layout on the deployment host.
