# runwell

**Pre-alpha / M0 bootstrap.** The workspace, configuration parser, and CLI contract
exist. Execution, protocol clients, and analysis commands are API skeletons.
Only `runwell version` and CLI help are operational; other commands print a clear
unimplemented message and exit with status 2. Do not deploy this milestone.

runwell is an open-source, single-binary Rust daemon for running GitHub Actions
jobs on one or a few owned Linux hosts, including VPS and bare metal. KVM is not
required. Persistent runner fleets can overcommit CPU and RAM, hide container
resource use, and leave short jobs waiting behind long work. runwell aims to make
host capacity and CI latency visible and controllable.

## How it works

A controller owns a scale-set message session for each size class, such as
`runwell-small` and `runwell-large`. It reserves real host capacity before creating
an ephemeral JIT runner. Without capacity, the job stays in GitHub's queue without
holding a runner. Messages are handled idempotently and acknowledged afterward.

The planned scheduler favors critical paths and short jobs using duration history,
shares capacity fairly across repositories and pull requests, and places work by
node headroom and class. Each node uses reservations with a PSI pressure brake,
systemd cgroup v2 limits, a per-job Docker API proxy for container attribution, and
an overlay workdir and HOME over immutable warm cache generations. Nodes exchange
commands with the controller over mutual TLS.

Runners launch `bin/Runner.Listener` directly, run one job from their own install
directory, and are always unregistered at the end. Final job measurements go to
SQLite; live metrics go to Prometheus. Only confirmed infrastructure failures may
receive a single retry; failing tests are never retried automatically.

## Commands

| Command | Intended responsibility | M0 status |
| --- | --- | --- |
| `runwell controller` | Scale-set sessions, scheduling, state, releases | Skeleton |
| `runwell node` | Linux admission, execution, reconciliation, drain | Skeleton |
| `runwell report` | Explain slow CI from GitHub API history | Skeleton |
| `runwell simulate` | Replay a real trace against scheduling policies | Skeleton |
| `runwell advise` | Workflow rules for agents: shards, serial hops, concurrency | Skeleton |
| `runwell version` | Print package version | Available |

## Goals

- Admit work only when CPU and RAM reservations fit, retaining OS headroom.
- Reduce queueing and critical-path latency with predictable fair sharing.
- Account for runner processes and Docker service containers per job.
- Recover from crashes, drain safely, and keep runner templates within the
  upstream 30-day update window.
- Explain resource use and workflow bottlenecks through reports and simulations.

## Non-goals

- A replacement for the GitHub Actions workflow engine or its runner binary.
- An arbitrary-code sandbox, public multi-tenant execution, or fork-PR hosting.
- Kubernetes orchestration, mandatory KVM, or persistent runner pools.
- Automatic retries of red tests or changes to workflow results.
- A cache server in M0; cache integration is future work.

## Development

Install stable Rust with rustfmt and clippy, then run:

```sh
# Put build artifacts on a volume with sufficient free space.
export CARGO_TARGET_DIR=/path/to/external-volume/cargo-targets/runwell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p runwell -- --help
```

Linux-only operations are gated so the whole workspace can be checked on macOS.
See [CONTRIBUTING.md](CONTRIBUTING.md) and [examples/runwell.toml](examples/runwell.toml).
The [architecture](docs/architecture.md) and [ADRs](docs/adr/0001-rust-single-binary.md)
describe the intended system, beyond the M0 skeleton.
[Dependency decisions](docs/dependency-decisions.md) record the exact pins and
validated crypto/runtime feature selections.

## Security

**Trusted repositories only. Never use with public repositories accepting fork
pull requests.** The daemon runs as root, jobs share the host kernel, and Docker
socket access is root-equivalent. The proxy attributes resources and does not
isolate hostile code. Read [SECURITY.md](SECURITY.md) before any deployment.

## License

Copyright The runwell contributors. Licensed under either the
[MIT License](LICENSE-MIT) or [Apache License 2.0](LICENSE-APACHE), at your option.
