# runwell-sim

Replay a `runwell-trace` JSONL file with discrete events and the same pure
scheduling/admission rules available to the daemon. The CLI never contacts GitHub.

```sh
export CARGO_TARGET_DIR=/Volumes/dev-disk/cargo-targets/runwell-m1-sim
cargo run --release -p runwell -- simulate \
  --trace /tmp/trace.jsonl \
  --hosts crates/runwell-sim/examples/hosts.toml \
  --policy all --format md
```

Use `--format json` for machine-readable output and `--seed 42` to override the
configured RNG seed. Keep private traces, configurations and generated reports
outside the checkout. Repository names are anonymized by default, in sorted
order. The example configuration and tests contain synthetic names only.

## Configuration and scenarios

See [examples/hosts.toml](examples/hosts.toml). CPU demand is an integer number of
cores; memory is GiB. Every prefix of `hosts` is evaluated: a two-host file produces
one-host and two-host scenarios. `jobs` provides exact-name demand overrides,
optionally scoped by repository; scoped entries take precedence. These are
reservations, not measured cgroup consumption. `host_class` restricts placement.
Set `derive_cpu_demand = true` to replace configured CPU cores with the trace
sensitivity proxy described below. RAM remains an explicit prior; this trace
format does not identify memory usage.

`contention_label` identifies jobs executed on the reference host. Jobs outside
that label contribute their observed external queue and execution time to their
workflow but never enter local runner, resource, contention, or failure accounting.
The contention fit assumes the selected label identifies **one reference host**.

Baseline pools match a repository, every configured label, and optional exact
`job_names`. The first matching
pool wins; an unmatched repository gets six runners. Each additional baseline
host gets the same pool limits, making that scenario an explicit runner-pool
replication experiment. Baseline placement uses the first available eligible host.
The optional semaphore is host-local and holds a runner while waiting; the
waiting job releases its CPU/RAM reservation until protected work starts.
`semaphore_steps` specifies acquire-step name fragments: recorded waits are
subtracted from local work, and the gate is reached after pre-acquire work.
`semaphore_release_steps` specifies release-step fragments. A slot is held until
the first matching release step completes, or job termination if none exists.
Using completion conservatively includes the release operation itself; trace
timestamps cannot identify the exact unlock inside that step. Cleanup after
release retains the runner and resources but frees the heavy slot. Each phase's
service work is integrated against the same fitted pressure curve as the whole
job. The legacy median estimator divides work in observed active-time proportions.
Without acquire timestamps the gate falls back to job start. Skipped steps are
not evidence of acquisition. Multiple acquire steps in one job are rejected
because this model supports one protected section, not nested/repeated locks.
Fail-open starts protected work without a token, timed from reaching the acquire
step; neither release nor completion can release someone else's token.
`semaphore_poll_seconds` defaults to zero (immediate FIFO wakeups). A positive
interval retries on each waiting job's clock, capped at the timeout deadline;
later arrivals can acquire before older waiters between polls. This models
polling without inventing shell overhead or random lock races. Acquisition wins
over timeout if a slot is free on the final attempt.

With `semaphore_history = true` (default), recorded step presence determines
whether a historical execution used the gate. An existing step list without an
executed acquire step disables the gate; an absent step list falls back
to the configured heavy class. `runner_history` can supply timestamped capacity
changes (`repo`, zero-based `host`, `at`, `runners`); first observed assignments
prove availability at that instant, not the activation time. A capacity
reduction drains occupied slots; an increase immediately wakes queued jobs.
Classic counterfactuals ignore history and apply their chosen gate throughout.

The keys above configure the `heavy` pool. Further named pools use
`[[semaphores]]` tables with `name`, `acquire_steps`, `release_steps`, `slots`,
`slots_per_host`, and optional `timeout_seconds` / `poll_seconds` (defaulting to
the global values). A job joins a named pool through `semaphore = "db"` in its
demand override; `heavy = true` still selects the heavy pool, and a job uses at
most one pool. Every pool has the same acquire, wait, poll, fail-open and release
rules, its own tokens on each host, and its own step-presence history. A step
matching two pools' fragments is an error. `[[semaphores.slot_history]]` entries
(`host`, `at`, `slots`) change one host's limit from a timestamp on: a reduction
drains held tokens, and waiting jobs see an increase at their next poll (or at
once without polling). Classic search's disabled mode and the equivalence check
turn every pool off. Reports list pool names and limits, never step names.

```toml
[[semaphores]]
name = "db"
acquire_steps = ["Wait for a database slot"]
release_steps = ["Release the database slot"]
slots_per_host = [2, 4]
[[semaphores.slot_history]]
host = 1
at = "2026-01-02T00:00:00Z"
slots = 1

[[jobs]]
name = "integration-db"
[jobs.demand]
semaphore = "db"
```

## Availability input

`--availability /tmp/availability.jsonl` accepts timestamped capacity changes and
half-open offline intervals `[start, end)`. A `.toml` file uses `[[events]]` tables
with the same fields. Alternatively, put `[[availability]]` tables in the hosts
configuration. All examples below are synthetic:

```jsonl
{"kind":"pool_size","host":0,"pool":0,"at":"2026-01-01T00:00:00Z","runners":2}
{"kind":"pool_size","host":0,"pool":0,"at":"2026-01-02T00:00:00Z","runners":4}
{"kind":"runner_offline","host":0,"pool":0,"runner":1,"start":"2026-01-02T01:00:00Z","end":"2026-01-02T01:05:00Z","cause":"broker"}
{"kind":"host_offline","host":0,"start":"2026-01-03T01:00:00Z","end":"2026-01-03T01:02:00Z"}
```

Host and pool indices refer to the configuration's `[[hosts]]` and `[[pools]]`
order. Pools must be explicitly configured and have local executions. Runner
ordinals are stable, zero-based identities within each host/pool; they must fit
its maximum configured/historical capacity. Size changes replace installed
capacity; zero disables dispatch. Before the first change, configured capacity
applies, so include an initial snapshot when needed. Do not mix `pool_size` with
legacy `runner_history` for the same pool. Duplicate size changes at one instant,
invalid indices and empty/reversed intervals are errors.

`runner_offline` means a listener cannot accept a new job, while its active job
continues. `cause` is `service` or `broker`, for provenance only. Overlapping
intervals are unioned by identity; an offline occupied runner does not also remove
another free slot. A shrinking pool drains active jobs before admitting more.
`host_offline` prevents dispatch and pauses all work on that host, retaining
reservations; this is a suspension model, not a crash/restart or failure model.
Overlapping host intervals also count once. Completions and cancellations precede
availability changes, then admission, at the same timestamp.

Baseline and `runwell-equivalent` respect runner and host availability. Resource
policies respect host outages only by default. They do not inherit historical
per-runner broker sessions, service restarts, or runner installation counts.
`--runwell-runner-availability` enables an explicit sensitivity combining resource
admission with those legacy slots and outages; it is not the default controller
model. A host with no supplied outage history is assumed online.

Classic searches use their chosen fixed counts instead of historical size changes.
On recorded hosts, additional slots cycle the observed runner ordinal outage
patterns with period equal to maximum configured/historical capacity. This is an
explicit counterfactual assumption; unobserved hosts have no inferred outages.
Reports include availability record count and sensitivity mode, never identifiers
or raw log records.

Prepare private adapters outside the checkout. A logged retry delay need not have
elapsed: cancellation can interrupt it immediately. First-failure-to-next-job
windows may include healthy idle time when successful empty polls are not logged.
Keep confirmed intervals and such upper-bound sensitivities separate, and do not
infer whole-host outages from listener network failures alone.

## Resource policies

Runwell has no fixed runner count. CPU/RAM reservations must fit physical capacity
multiplied by their respective overcommit factors. Both default to 1.0. Placement
maximizes the smaller post-placement CPU/RAM headroom fraction, with stable host
order breaking ties. Available policies are `fifo`, `shortest`, `critical-path`,
and `fair-share`; `runwell` aliases `critical-path`. Fair share orders repositories
by CPU core-seconds received since trace start. At `aging_seconds`, a job precedes
all younger jobs in FIFO order. If the oldest aged job cannot fit, one eligible
host drains instead of continually backfilling smaller jobs on it.

`runwell-equivalent` uses the exact baseline FIFO placement and per-pool runner
limits, with no heavy gate. Every comparison containing it includes a job-by-job
timing, placement and failure sanity check against baseline with the gate off.
Use `--overcommit-sweep` (or TOML `overcommit_sweep`) to evaluate all four resource
priorities at 1, 1.25, 1.5, 2, 2.5 and 3 times CPU and RAM capacity. Runner-count
policies appear once per scenario.

## Replay and fitting

All observed run attempts are replayed as background load. By default, reported
cohorts contain successful runs whose latest observed attempt is one, grouped by
repository and event. This avoids treating a failed initial attempt as successful
when a later rerun updates its run-level conclusion. Workflow imports also scope
reporting to the selected workflow; other workflows still contribute load.
Set `successful_first_attempt = false` to report all attempts instead. Recorded
conclusions determine cohort membership; simulated failure draws do not select
which runs are included.

Explicit `needs` names are resolved within a run attempt, with every matching
matrix job required. Missing dependencies or cycles are errors. `needs = []`
means a root; observed runner queue is not replayed as an arrival delay. Unknown
`needs` are inferred from the latest positive-work completion at or before job
creation. Measured creation-to-predecessor gaps are retained as dispatch latency.
This cannot recover the original workflow DAG. Import private workflow files:

```sh
runwell simulate --trace /tmp/trace.jsonl --hosts /tmp/hosts.toml \
  --workflow-needs example/app=/tmp/workflow.yml \
  --workflow-since example/app=2026-01-01T00:00:00Z \
  --policy all --overcommit-sweep --classic-search --format json
```

Repeat `--workflow-needs REPO=FILE` for each repository. The importer resolves job
IDs, display names, matrix name templates, `needs`, and integer `max-parallel`.
The matrix limit is shared across hosts. A graph applies only to matching workflow
runs at or after `--workflow-since`; structurally incompatible or older runs retain
timestamp inference, while recognized job classes retain declared matrix limits.
When a matrix limit itself changed historically, use versioned/enriched trace
metadata instead of assuming the current limit. Matrix children use the earliest creation gap so their
observed throttle is not charged twice. Unsupported concurrency expressions fail
explicitly; workflows are parsed, never executed.

Cancellation groups support common workflow plus PR/branch forms and boolean or
PR-only `cancel-in-progress`. PR IDs are absent from some traces, so imported groups
use branch identity, separated by PR versus branch event. First job creation is
the default concurrency-admission clock (`cancel_on_dispatch = true`), because
GitHub may dispatch near-simultaneous runs out of creation order. Run creation
still starts end-to-end latency. `cancel_on_dispatch = false` enables the creation
clock for sensitivity testing. Reruns use their first new job creation. Superseding
a run frees active and gate-held runners, reservations and matrix slots; cleanup
latency is configurable with `cancel_grace_seconds`. All these rules call shared
pure scheduler functions. The original PR identity, exact group-admission event
and historical workflow concurrency revisions remain evidence limitations.

Missing or impossible timestamps are counted and excluded. Skipped jobs consume
zero work. A cancelled record with no runner and no steps consumes zero work and
retains its observed terminal time. An identical execution repeated in a later
attempt snapshot depends on the original execution, without running again.
Execution identity includes repository, run, name, runner and start/end
timestamps; different attempts are required for reuse. Snapshot job IDs can
change, and reused executions may predate the later attempt creation clock.

Observed concurrency is the time-weighted integral of overlapping local runner
active intervals, after removing recorded semaphore waits, nonexecuting
cancellations and reused executions. A waiting runner occupies its pool but does
not contribute CPU demand. Concurrency is averaged over each job's active work
intervals; `exclude_semaphore_waits_from_contention = false` reproduces the older
runner-occupancy fit for diagnostic ablations.
Successful low-concurrency durations establish a median for each repository/job
name to fit slowdown. Service work is then estimated by integrating the fitted
speed over each observed active execution interval, using reference-host aggregate
CPU demand and configured RAM priors. Forward replay applies the same pressure
curve to that work under each new schedule. This preserves per-execution input/cache
variation and avoids applying contention twice to interrupted jobs. It does not
preserve observed queue waits or fit latency targets. Names without low-load
successes have a unit CPU curve and are counted as unsupported.

`preserve_work_variation = false` retains the legacy diagnostic estimator: contended
successes use the low-load name median; low-load and interrupted executions keep
observed work. `observed_work = true` bypasses both decontending and forward slowdown.

By default `fit_by_class = true` fits each repository/workflow job class separately;
`false` retains pooled fitting for diagnostic ablations. The CPU curve uses medians of successful observed/intrinsic duration ratios by
rounded concurrency, then weighted isotonic regression to obtain monotone
piecewise-linear inflation. Speed is its reciprocal. Aggregate configured CPU
demand / host cores is mapped to equivalent reference-host concurrency using
work-weighted average cores per job. The curve is flat below the low-concurrency
threshold and clamped beyond measured support. All running jobs' remaining work
is integrated again whenever arrivals, completions, or semaphore timeouts occur.
Completions and zero-work dependency closure happen before admissions at the same
instant; identity breaks remaining ties deterministically.

The optional CPU proxy is `round(reference cores / peak observed local concurrency
* class inflation at high concurrency)`, clamped to 1 through reference cores.
It assigns higher reservations to more contention-sensitive classes. The curves
are then refitted against aggregate observed proxy CPU demand, normalized by
mean cores per job, so fitting and replay use the same load axis. Reservations
are inferred once, without iterative target fitting. `fit_proxy_load = false`
retains the older raw-count axis for diagnostic ablations. Output
includes every anonymized class fit and its sample support. Absolute CPU demand
is not identifiable from durations alone: this peak-concurrency anchor is an
assumption, not a CPU measurement. Unknown classes have a unit slowdown curve.
Avoid interpreting an overcommit optimum as a measured hardware limit.

Above `memory_threshold`, speed is divided by
`1 + max(0, memory_ratio / threshold - 1) * (memory_penalty - 1)`.
Heavy jobs' low/high all-cause failure fractions are fitted from the trace. CPU or
memory stress interpolates between those fractions, without decreasing risk if
sampling gives a lower high-load rate. A common seeded per-job draw is compared
with its time-weighted exposure. Missing failure bins have no empirical support,
which is visible in the output. These are **infra-failure proxies**, not identified
OOM or infrastructure failures. The trace cannot identify a separate memory
failure effect; the speed penalty is a configurable assumption. No retry or
failure-driven downstream cancellation is simulated.

## Metrics and calibration

End-to-end latency is run creation to the last simulated job completion. Quantiles
use linear interpolation (R type 7). Queue share sums runner and semaphore waits
along each run's simulated critical predecessor chain and divides by summed
end-to-end time. Dispatch gaps and dependency execution are not queue time.
CPU/RAM utilization are demand-based physical-capacity integrals over the entire
replay window, including idle time and background runs. They are fleet metrics,
so their values repeat across repository/event rows. Failure counts in each row
cover its reported cohort; JSON also includes whole-replay failures and fail-opens.

The one-host baseline compares p50, p90 and observed critical-path queue share
with matching observed cohorts. It prints signed latency errors, queue-share
error in percentage points, and an explicit 10% latency pass/miss. Observed
successes cancelled in replay are counted separately and excluded from completed
latency quantiles. Any such censoring fails calibration, even if both quantiles
are within tolerance; never mistake losing slow runs for an improvement. A miss does not
rescale durations, arrivals, resource demand, or output. Treat scenarios as
exploratory until calibration passes. Missing workflow edges, runner availability,
dispatch rules, cancellation behavior, resource measurements and per-job
contention sensitivity can prevent a defensible match. If an external baseline
uses a different date range or cohort, compare that separately as well.

## Persistent-runner allocation search

`--classic-search --classic-max-runners 12,8 --target-p90 25,20` exhaustively
evaluates integer runner allocations per repository and host inside the stated
per-host total bounds, including leaving a host idle. Omit targets to use half
the observed PR p90, in anonymized repository order. The reusable
`search::search_allocations(&PreparedTrace, &SearchOptions)` has no filesystem or
CLI dependency and returns the top five per semaphore mode. It ranks by the
maximum repository p90/target, then total runner count and allocation order.
Infeasible candidates and candidates cancelling any selected observed successful
PR are rejected. Counts, bounds and targets are reported, so an empty result
cannot masquerade as a valid recommendation. This is a bounded optimum, not a
claim about all possible runner counts. The initial API requires one pool per
repository; more complex label partitions are rejected. Search has a 100,000-grid
limit and uses deterministic parallel workers. The five-second replay target
does not include thousands of independent allocation-search replays.

## Validation

Synthetic unit tests cover the contention fit, event ties, dependency closure,
semaphores, host pins, external-host isolation, input artifacts, and deterministic
failures. Proptest checks reservation bounds, aging and eventual selection, and
p90 monotonicity when adding a host to independent homogeneous-resource bursts
under every runwell priority. General heterogeneous DAG scheduling can have
resource/list-scheduling anomalies; the extra-host property is intentionally not
claimed for arbitrary traces.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

For timing, build release first, then time the binary with `--policy all`; include
trace parsing, model fitting and all scenarios in the measurement. Runtime is
proportional to events and the ready/running sets, with an event heap and indexed
DAG edges; observed concurrency queries use a prefix integral and binary search.

`heavy_slots_per_host = [2, 1]` overrides the global `heavy_slots` in host order.
Missing entries inherit the global limit. The enabled semaphore mode in allocation
search retains these overrides; the disabled mode clears every host's semaphore.
Per-host pool sizes can be supplied as `runner_history` changes at the start of the
trace; allocation search replaces those installed sizes with each candidate split.

Add `--target-p50 8,6` to the classic search to include median goals alongside the
p90 goals. Ranking then minimizes the worst ratio across both requested percentiles
and every repository. Without this option the objective remains p90 only; passing
that objective does not imply the medians meet a latency goal.
