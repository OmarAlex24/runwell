# M5a integration contract

M5a supplies scheduling, history, retry and observability APIs. It does not change
`runwell-node::Controller`, start background tasks, or send live notifications.
The controller/integration owns the wiring described below.

## Scheduler

`runwell_scheduler::Production` implements the existing `SchedulingPolicy` trait.
Use `Production::new(&config, &snapshot, &fairness)?.decide(jobs, nodes, now)` to
receive both a placement and its proposed `FairState`. `select` returns only the
placement, for compatibility; a production caller must carry the fairness state
between accepted reservations.

- `PendingJob` and `NodeHeadroom` remain source-compatible with earlier policies.
- Supply `ProductionSnapshot.jobs` with `HistoryKey` (canonical repo, workflow
  path/YAML job key, reservation class), PR identity, run/attempt identity, and
  optional `Criticality`. Missing job or node metadata fails closed.
- Compute `graph_criticality(needs)` once per known run DAG. It returns longest
  downstream depth and distinct transitive fan-out, rejecting invalid DAGs.
- `NodeHeadroom.capacity` is admission's **already scaled allocatable limit**,
  after OS/daemon headroom; `reserved` includes all live reservations. There is no
  second overcommit multiplier. Headroom-only reporters can use available
  resources as capacity and zero reserved resources.
- `NodeStatus` reports accepted classes, remaining job slots, current run
  occupancy, and `admission_open`. Set it false for stale, offline, drained or
  PSI-paused nodes. Authoritative host reservation acceptance remains mandatory.
- Commit the returned fairness state only after host acceptance; discard it on
  rejection and refresh the snapshots. Persist/recover `FairState` with controller
  state if continuity across restart is desired. It derives serde traits.

Ordering is aged FIFO first, then smooth weighted round-robin across active
repositories and PRs, then descending depth/fan-out within the chosen PR, then
ascending learned p50, p90 and arrival/ID. Unknown DAG shape uses workflow-job
history. Weights represent **admissions**, not CPU-seconds. Inactive queues do not
accumulate credit, and aging overrides still charge their fairness accounts.

Placement maximizes the minimum remaining CPU/RAM fraction after reservation.
When that fraction is at/below `tight_headroom`, a node already hosting the run
is less preferred. Stable node IDs break ties. The oldest feasible aged job
protects one eligible host from refill if it needs that host to drain.

The aging bound is the time at which FIFO/drain protection starts. An absolute
wall-clock start guarantee is impossible without bounded running-job duration,
finite older backlog and eventual eligible capacity. With those assumptions,
younger arrivals cannot indefinitely postpone an aged job. An impossible job
cannot block protection for a feasible one.

Property tests exercise input permutation determinism, sequential capacity and
job-count bounds, FIFO protection against an adversarial short/critical stream,
large-job draining, and convergence to configured repository and PR weights.
Unit tests cover class/availability constraints, headroom, anti-affinity, invalid
DAGs and historical criticality.

## Learned history and journal

`Store::record_completion(CompletedJob)` journals duration, queue time, conclusion,
class and graph shape idempotently by repo/GitHub job ID/attempt. Use authoritative
GitHub completion and the actual execution binding, not runner exit code or the
original acquired request identity.

A writer transaction recomputes only the affected key's latest 128 successful,
positive-duration executions, ordered by completion time/job ID/attempt. It stores
nearest-rank p50/p90 and latest successful graph shape in `duration_estimates`.
Failures remain auditable but do not bias execution-duration estimates. Queue
time is never included in learned runtime. Out-of-order events and redelivery
cannot duplicate the window.

Load `Store::duration_history(class_defaults)` once, then use the indexed
`Store::duration_estimate(&key)` after a completion to update just the changed
entry. Pure scheduling uses `HistorySnapshot::estimate`, with O(log keys) lookup
and no database I/O. Supply class p50/p90 defaults for cold starts; the policy can
also use the caller's `PendingJob.expected_seconds` when no estimate is supplied.

Migrations are `0100_job_history.sql` and `0101_retries.sql`. Store changes are
isolated to new modules plus module declarations and three single-writer mutation
arms; merge these with M5b's `0200_*` migrations and writer additions.

## Classification and retries

`Classifier::builtin()` loads the shipped [rule table](../crates/runwell-retry/src/rules.toml).
`Classifier::from_toml` permits custom rules but always retains built-in test/code
and ambiguity vetoes. Literal patterns and bounded, precompiled diagnostic regexes
are case-insensitive; GitHub timestamps and terminal colors are normalized before
matching framework summaries. Structured `CodeFailure` wins
unconditionally, including over OOM and runner-loss signals. Supply that signal
from known red-test/lint/build results; plain exit 1/137 is insufficient evidence.

| Rule | Positive infra evidence |
| --- | --- |
| `oom` | Attributed job-slice OOM kill |
| `runner-crash` | Runner crash/loss, lost node, shutdown/communication messages |
| `docker-daemon` | Structured daemon error or inability to connect to Docker |
| `pressure-timeout` | Timeout overlapping host PSI above the configured brake |
| `never-picked-up` | Runner pickup deadline expired without job pickup |
| `disk-full` | No space left on device |
| `registry-network` | Specific TLS/registry/connection timeout messages |

`test-or-lint` vetoes framework failure records, positive failure counts and actual
assertion/lint/compile diagnostics. Bare `clippy`/`eslint` command names and zero
failure counts are not code evidence. `ambiguous-application-error` keeps
unattributed network exceptions and tracebacks `Unknown`, ahead of infra rules,
even with custom tables. No positive infra signal means `Unknown`; generic
failure/timeouts do not authorize a retry. Success,
cancellation, skipped, neutral and unrecognized conclusions cannot classify infra.
Rules cannot prove that a missing/truncated log contained no red tests: integration
must provide structured code-result signals as well as available annotations/tail.

`RetryPolicy` defaults **off**. `retry(policy, classifier, journal, api, request,
now_unix)` accepts `RetryJournal` and `RetryApi` trait objects, implemented by
`Store` and `RestClient`. It fetches the latest run and every attempt job, verifies
all non-success/skip/neutral jobs have matching infra evidence, rechecks the latest
attempt, atomically claims, then performs one logical rerun operation.

[GitHub's rerun-failed-jobs endpoint](https://docs.github.com/en/rest/actions/workflow-runs#re-run-failed-jobs-from-a-workflow-run)
reruns all failed jobs and dependents. Mixed infra/test or unknown failure sets are
therefore ineligible. Missing evidence, incomplete inventory and stale attempts
are also rejected. There is no conditional attempt/POST primitive, so a concurrent
external manual rerun cannot be atomically excluded by the final recheck.

The store claim enforces at most one logical rerun operation per run attempt
(thus per failed job attempt), blocks immediate automatic retry chains, and charges the per-repository
UTC-day cap **per failed job**, atomically. Claims survive restart and all send
outcomes: accepted, rejected, ambiguous, or a crash before/during sending. No
ambiguous POST is automatically resent or refunded. This trades possible missed
retries for avoiding duplicate accepted mutations; it does not claim exactly-once
remote execution. An App-token HTTP 401 is a definitive authentication rejection:
the REST client invalidates only that rejected token and shares one cache refresh
with concurrent requests, then replays once. A late 401 never evicts a newer token.
A second 401 invalidates the replacement for future calls but ends this operation.
PAT credentials have no automatic refresh source and are not replayed on 401.
Use `retry_record` for audit/reconciliation. Do not bypass `retry` with raw POSTs.

`RestClient::new` / `from_config` support existing App/PAT file/env configuration.
App JWTs use RS256, a 60-second skew allowance and a 600-second total lifetime;
installation-token caching is single-flight with a 60-second refresh margin.
HTTP requests have timeouts, no redirects, no implicit retries, and sanitized
errors. The sole explicit replay is bounded App-token 401 recovery; transport
failures and 5xx never trigger mutation replay. GitHub.com and enterprise API
bases are supported. Wiremock tests cover
PAT/App exchange, JWT signature, caching, pagination, HTTP failures and redirects.
The App/PAT needs Actions write permission for reruns and read access to jobs.

## Metrics and alerts

Construct `Metrics::new(classes, nodes)` from bounded configured allowlists and
serve `Metrics::encode()` through the controller's scrape route. Unknown labels
collapse to `other`; repository, PR and job identities never become metric labels.
Wire event adapters to `completed`, `admission`, `pressure`, `retry`, `headroom`
and `infra_failure_ratio`. Count accepted retries per failed job/class, not per
network attempt, and deduplicate authoritative completion events before counting.

Registered families: `runwell_queue_seconds`, `runwell_run_seconds`,
`runwell_admission_decisions_total`, `runwell_psi_percent`,
`runwell_retries_total`, `runwell_completed_jobs_total`,
`runwell_infra_failure_ratio`, `runwell_node_headroom_cpu_slots`, and
`runwell_node_headroom_memory_bytes`.

`evaluate(AlertConfig, AlertSnapshot, now)` evaluates:

- Infrastructure failures strictly above 1% in the default rolling hour (minimum
  sample count is configurable; completion identities deduplicate deliveries).
- Queued/running job age over 3× learned p90 by default. Use ready/start timestamps
  and different phase identities if watching both phases.
- Node report older than 90 seconds, including registered nodes with no report.
- Runner template expiry within three days (including already expired templates).

Feed the same window's `AlertSnapshot::infra_ratio` into the metric gauge.
`Webhook::sync` accepts the complete active alert set; resolved conditions cancel
pending retries. Drive `deliver_due(now, limit)` from a controller task. Delivery
is JSON POST with timeout, bounded exponential backoff, Retry-After support,
stable retry keys, cooldown and an outbox-size bound. It retries transport errors,
408, 429 and 5xx; other HTTP failures exhaust that notification. No sleeps are
hidden in ticks. HTTP awaits should run separately from critical admission work.
Webhook state is in-process: receivers should honor `Idempotency-Key` to handle
lost responses, and controller integration can persist alert state if restart
cooldown continuity is required. Payloads contain identities/values, never logs.

## Simulator and validation

`runwell simulate --policy production` and `--policy all` run the new policy with
existing calibration, contention, cancellation, availability and report machinery.
The original `runwell` alias remains critical-path for compatibility. Simulator
PR identity uses branch (or a per-run bucket when absent), because the trace schema
has no PR-number field. Replay completions teach a 128-success duration window;
initial work estimates use the simulator's existing calibration. Readiness passed
to the policy is normalized only for events consumed within the simulator's
`EPS` tolerance; production scheduling retains a strict readiness clock.

On the existing `crates/runwell-advise/tests/fixtures/timing.jsonl` with
`crates/runwell-sim/examples/hosts.toml`, both baseline and production have E2E
p50/p90 **10.0/10.0 minutes**, on both one and two hosts. Queue share is zero for
both. This single-job fixture has no contention. Its observed E2E is 12 minutes;
the existing report correctly flags the -16.7% calibration error. It provides no
evidence of a 2× improvement.

The existing CLI smoke fixture (`simulate_emits_parseable_json_and_markdown`)
has baseline and production p50/p90 **1.0/1.0 minutes**, with passing calibration.
Existing simulator fixtures using `Policy::ALL` also exercise production against
outages, completion ordering, seeded reporting and capacity changes. Real PR
traces with representative bursts and accurate node/class reservations remain
necessary to measure the project's latency target.

Initial validation completed on 2026-10-01 (before independent-review fixes):

- macOS: `cargo fmt --all --check`, workspace clippy with `-D warnings`,
  `cargo test --workspace` (305 passed), and `cargo deny check` passed.
- Linux: workspace clippy with `-D warnings` and workspace tests passed in one
  `rust:1.98.1` container, started in the foreground under
  `lockf -k /Volumes/dev-disk/projects/runwell-coord/docker.lock` with
  `--memory 12g`, `CARGO_BUILD_JOBS=6`, and `CARGO_PROFILE_TEST_DEBUG=0`.
  307 tests passed; 14 existing privileged systemd/Docker/overlay tests retained
  their repository `#[ignore]` markers. The `--rm` container exited successfully.
- No commits or pushes were made.

Independent-review fixes validated on 2026-10-01:

- Framework failure summaries and actual lint diagnostics veto infra rules;
  ambiguous application/network text stays unknown. Regression tests cover
  pytest, other frameworks, timestamped/colored logs, OOM/shutdown during lint
  commands, and a red pytest job making no retry claim or HTTP POST.
- Simulator readiness regression reproduces an arrival consumed within `EPS`
  and verifies production completes with the same timings as FIFO.
- App-token tests cover concurrent and late 401 responses, one bounded replay,
  repeated rejection, no ambiguous mutation replay, and one durable retry claim.
- macOS: fmt, workspace clippy with `-D warnings`, workspace tests (317 passed),
  and cargo-deny passed. `git diff --check` passed.
- Linux: workspace clippy with `-D warnings` and workspace tests passed in one
  foreground, locked `rust:1.98.1` container with the same limits above. 319 tests
  passed; 14 existing privileged tests remained ignored. The container exited
  successfully and was removed. No commits or pushes were made.
