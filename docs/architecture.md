# runwell architecture

Status: accepted target architecture for a pre-alpha implementation. M0 contains
public API skeletons, configuration validation, and CLI help/version behavior.
It does not yet run jobs, communicate with GitHub, or operate host resources.

## System boundary

runwell is a single Rust binary with controller and node modes, deployed on one
or a few owned Linux hosts. It executes trusted GitHub Actions workloads using
the upstream runner binary; no KVM or Kubernetes is required. report, simulate,
and advise are analysis modes of the same executable.

```text
GitHub Actions scale-set queues + public REST API
                    |
                 controller ---- SQLite journal
                    |        ---- Prometheus live metrics
                   mTLS
                    |
              node(s) on owned Linux hosts
                    |
                 ci.slice
                    |
             rw-j<id>.slice (logical per-job slice)
                    |-- runner service: bin/Runner.Listener run
                    |-- Docker scopes via per-job API proxy
                    `-- overlay workdir and HOME over warm generations
```

The job slice is specified as `rw-j<id>.slice` under `ci.slice`. Actual transient
unit naming must respect systemd's dash-based slice ancestry when mapping these
logical names under the CI parent. Systemd owns the hierarchy and its properties;
runwell reads counters rather than writing systemd-managed cgroup files directly.

## Controller

The controller owns one scale-set message session per size class, such as
runwell-small and runwell-large. It holds a local single-instance lock and closes
sessions on graceful shutdown. On startup it reconciles the durable registry
before opening sessions, and uses initial absolute statistics before polling.

Each message is processed before acknowledgment, and every handler is
idempotent. Repeated acquisition is harmless; runner and request identities are
durable deduplication keys. Unknown message types are recorded and acknowledged
without wedging the queue. Poll statistics drive demand because message bodies
can be truncated. Admin and message-queue tokens are separate, refreshed through
single-flight paths; a 401 causes one refresh and one retry of the failed call.

A JIT runner is created only after a node has room and accepts a reservation.
Without room, the job waits in GitHub's queue without holding a runner. Capacity
advertised by each session reflects realizable class capacity. The controller
selects available requests to acquire, persists intent, prepares the reserved
host, then generates and launches a runner. GitHub ultimately assigns a matching
job to an idle runner in the class; the scheduler cannot force an arbitrary
request-to-runner binding. Actual execution is bound from JobStarted events.

Scheduling is pure: critical path and short jobs first, with durations learned
from history, fair share across repositories and pull requests, and placement
across nodes by headroom and class. Policy inputs are snapshots. Host reservation
acceptance is authoritative and rechecked before JIT creation.

The controller also owns:

- A runner release manager: daily release checks, SHA-256 verification, immutable
  template promotion for new jobs, and enforcement of the 30-day update window.
  Outdated runner signals pause creation and advertise zero capacity until fixed.
- An infrastructure-failure classifier and a single automatic retry for confirmed
  infrastructure failures. Test failures and unknown causes are never retried.
  A process exit code alone does not classify the workflow result.
- A sqlx SQLite journal using WAL and one writer connection, storing requests,
  reservations, runner registrations, transitions, history, and final measurements.
- Prometheus metrics with bounded labels for live headroom, queueing, pressure,
  lifecycle errors, and release age; durable job measurements remain in SQLite.

## Node

One node serves each host. It admits by reserved CPU and RAM after reserving OS
and daemon headroom. PSI is a brake with hysteresis: crossing an upper threshold
pauses admission, while sustained recovery below a lower threshold resumes it.
Pressure never manufactures capacity beyond reservations.

Before execution, the node creates a systemd transient `rw-j<id>.slice` under
`ci.slice`, with MemoryHigh, MemoryMax, MemorySwapMax=0, and CPUWeight. It enables
IOAccounting and may add task, runtime, and I/O ceilings. The limited slice must
exist before Docker references it, to avoid implicit unlimited slice creation.

Each job has a Docker API proxy on its own Unix socket, passed as DOCKER_HOST.
Container creation forces cgroup-parent and job labels; network creation adds
labels. Docker socket bind sources are rewritten so container jobs also reach
the proxy. Attach, exec, and BuildKit upgrade/hijack streams must pass through.
The runner account has no direct Docker socket access. The proxy attributes
resources, including Docker service containers, and is not an isolation boundary.

Each job has an overlay workdir and HOME on warm cache generations, along with
its independent runner install root. Lowers are immutable while mounted and
remain referenced until jobs finish. Overlay mounts use redirect_dir=on and live
in the host mount namespace so Docker can resolve bind sources. Shared mutable
caches are separate from immutable lower generations.

The node reads per-job cgroup stats before teardown: CPU, memory peak/events,
I/O, PSI, and task counts. Startup reconciles systemd units, Docker labels, mount
state, and the durable registry. Restarts are drain-safe: stop admission, preserve
live runner units and reservations, reconnect/reconcile, then resume. A transient
unit surviving the daemon is not treated as an orphan solely because of restart.

## Transport

Controller-to-node communication uses mTLS, authenticating both ends and binding
node identities to certificates. Reservations and lifecycle operations carry
durable identities and must tolerate duplicate delivery. Reject unauthenticated
peers and stale node snapshots. Certificate renewal and reconnect must not
invalidate live job accounting. Transport RPCs are reserved for a later milestone.

## Runner and lifecycle

Every runner receives a separate install directory cloned from a SHA-256-verified
immutable template. Spawn `bin/Runner.Listener run` directly with
ACTIONS_RUNNER_INPUT_JITCONFIG and
ACTIONS_RUNNER_RETURN_VERSION_DEPRECATED_EXIT_CODE=1. Never use run.sh wrappers
that rewrite exit codes, or put JIT credentials in argv. Disable runner self-update;
the release manager updates templates without modifying running installations.

Persist a unique runner name before JIT generation, then its returned agent ID
before spawning. Reconcile ambiguous POST outcomes by looking up that name.
The runner executes one job and exits. Process exit authorizes local cleanup;
JobCompleted gives the authoritative workflow result. An exit of zero does not
prove a job ran. Always send DELETE agent at the end, including successful exits.

For idle removal, DELETE agent first: 204 or 404 permits stopping the process;
409 JobStillRunning preserves it. Other failures remain in the durable cleanup
queue and are retried with backoff. Never kill an idle candidate before the
service has confirmed it cannot receive another job.

```text
Queued -> Admitted -> Prepared(workspace, slice, proxy)
       -> Running -> Draining -> Measured -> Cleaned
```

Teardown is idempotent: establish runner exit, attempt agent deletion, remove
job-labeled containers and networks, read and journal final cgroup counters,
stop the slice, unmount overlays, release generation references and reservations.
A busy-agent response defers destructive cleanup and triggers reconciliation.
Measurements must survive a crash between sampling and slice removal.

## Analysis commands and crate boundaries

| Crate | Responsibility |
| --- | --- |
| runwell | CLI and eventual controller/node task wiring |
| runwell-config | TOML schema, validation, credential file references |
| runwell-github | REST, App JWT/PAT auth, installation tokens |
| runwell-scaleset | auth/api/listener/supervisor/runner protocol-port boundaries |
| runwell-scheduler | Pure priority, fairness, and node placement |
| runwell-admission | Pure reservations and PSI brake decisions |
| runwell-node | Linux systemd, cgroup, PSI, reconciliation, drain |
| runwell-dockerproxy | Per-job Docker attribution and stream forwarding |
| runwell-workspace | Overlay workdir/HOME and immutable generations |
| runwell-runner | Verified templates, release manager, direct JIT launch |
| runwell-store | sqlx SQLite journal and measurements |
| runwell-metrics | Prometheus live metrics |
| runwell-advise | Workflow rules: shardable steps, serial hops, concurrency |
| runwell-sim | Deterministic real-trace replay against scheduling policies |
| runwell-report | GitHub API history explaining CI latency |

The scale-set module layout follows the proposal in
[the protocol research](research/SCALESET_PROTOCOL.md). Its runner module marks
protocol-side process/updater boundaries; host execution belongs to runwell-runner.
The [stack research](research/RUST_STACK.md) supplies dependency choices.
All Linux operations and Linux dependencies are target-gated. Pure scheduling
and admission must never acquire I/O dependencies. All crates inherit forbidden
unsafe code and workspace clippy warnings; CI denies warnings.

## Security and exclusions

Trusted repositories only. The root daemon, shared kernel, privileged Docker API,
and administrative GitHub credentials require a single-tenant trust model. Never
run public repositories that accept fork pull requests. See [SECURITY.md](../SECURITY.md).
M0 adds no runner persistence, cache service, microVM isolation, or deployment units.
