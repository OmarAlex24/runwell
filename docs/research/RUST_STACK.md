# CI engine: Rust stack, host capabilities, risks, workspace layout

Researched 2026-09-30. Crate versions come from the crates.io API on that date. Host facts come from read-only SSH to `root@<reference-host>`. Runner facts come from `actions/runner` @ `ca43437` (2026-09-30).

---

## 1. Host capability findings (<reference-host>)

**Correction to the brief: the host runs Debian 13 (trixie), not Ubuntu 24.04. It is a KVM guest with no nested virtualization.**

| Area | Finding | Implication |
|---|---|---|
| OS / kernel | Debian 13.6, kernel `6.12.101+deb13-amd64` | Modern enough for PSI triggers, the new mount API with `lowerdir+` (≥6.8), and per-fd `memory.peak` reset (6.12) |
| CPU | AMD EPYC 9645, 12 vCPU, `hypervisor` flag set, **no `svm`/`vmx`**, `systemd-detect-virt` = `kvm` | **No `/dev/kvm`, so Firecracker, Cloud Hypervisor and microVMs are out.** Isolation stops at cgroups and containers |
| Memory | 31 GiB RAM, 8 GiB swapfile **with 6.4 GiB in use**, `vm.swappiness=10`, THP `always` | Memory PSI will lag behind actual swapping. Job slices should set `MemorySwapMax=0` |
| cgroups | Pure cgroup v2 (`cgroup2 ... nsdelegate,memory_recursiveprot`). Root controllers `cpuset cpu io memory hugetlb pids rdma misc`, all enabled in `subtree_control` | Every controller we need is available. `memory_recursiveprot` makes `MemoryLow` on a parent slice protect its children |
| systemd | 257.13. `DefaultIOAccounting=no`. **`systemd-oomd` is inactive** | Set `IOAccounting=yes` explicitly on job slices. OOM policy is ours to own (kernel OOM inside `MemoryMax`) |
| PSI | `/proc/pressure/{cpu,io,memory}` present, `CONFIG_PSI=y`. Per-cgroup `*.pressure` files exist | Triggers are usable at system and slice level |
| Disk | virtio `vda`, **ext4** root (1007G, 438G free). Scheduler `[none]`. `io.cost.qos` is empty (iocost disabled) | **No reflinks on ext4.** `IOWeight=` is a no-op unless iocost is enabled (`echo "254:0 enable=1" > /sys/fs/cgroup/io.cost.qos`) or BFQ is used. `io.max` (bandwidth/IOPS caps) works regardless |
| Filesystems | `overlay`, `xfs`, `btrfs` all available as modules. **`xfsprogs`/`btrfs-progs` are not installed.** `OVERLAY_FS_REDIRECT_DIR`, `INDEX`, and `METACOPY` are off by default | Overlayfs works now. Reflinks need a loop-mounted XFS/btrfs image plus the progs. Directory renames on overlay return `EXDEV` unless you mount with `redirect_dir=on` |
| Docker | Engine 29.7.2 (API 1.55), containerd v2.3.3, runc 1.4.3, **Cgroup Driver: systemd**, cgroup v2, storage `overlayfs` (containerd snapshotter). `daemon.json` only has builder GC | `--cgroup-parent` has to be a **systemd slice name** (`ci-j42.slice`), and dockerd creates a `docker-<id>.scope` under it |
| Other runtimes | No sysbox, podman, crun or fuse-overlayfs. `rootlesskit` is present | Plain runc only |
| Current CI | **10 persistent runners** (6 repo-a, 4 repo-b) as systemd services under `/opt/actions-runners/*`, user `actions` (in group `docker`), runner **2.337.0** (self-updated from the 2.336.0 tarball). `.env` sets `TURBO_CACHE_DIR=/var/cache/ci/turbo` and `GOMAXPROCS=4` | Jobs use **testcontainers (ryuk)** plus custom `custom-postgres-ci:*` images, so they reach the Docker API directly, not only through the runner's CLI |
| Load at sample time | load avg 24 on 12 vCPU. `cpu some avg10=48%`, `io full avg10=1.5%`. vmstat shows ~1% steal | The existing fleet already oversubscribes the host. Admission control is worth having |

Raw snippets:

```
$ cat /etc/os-release | head -3
PRETTY_NAME="Debian GNU/Linux 13 (trixie)"
$ uname -r
6.12.101+deb13-amd64
$ ls -l /dev/kvm
ls: cannot access '/dev/kvm': No such file or directory
$ grep -c -wE "svm|vmx" /proc/cpuinfo ; systemd-detect-virt
0
kvm
$ mount | grep cgroup
cgroup2 on /sys/fs/cgroup type cgroup2 (rw,nosuid,nodev,noexec,relatime,nsdelegate,memory_recursiveprot)
$ cat /sys/fs/cgroup/cgroup.subtree_control
cpuset cpu io memory hugetlb pids rdma misc
$ systemctl --version | head -1
systemd 257 (257.13-1~deb13u1)
$ cat /proc/pressure/cpu /proc/pressure/memory /proc/pressure/io
some avg10=48.44 avg60=26.36 avg300=13.28 total=135471894567
full avg10=0.00 avg60=0.00 avg300=0.00 total=0
some avg10=0.00 avg60=0.00 avg300=0.00 total=1351331889
full avg10=0.00 avg60=0.00 avg300=0.00 total=926959313
some avg10=6.09 avg60=3.60 avg300=1.86 total=39572373856
full avg10=1.52 avg60=1.78 avg300=1.24 total=34314853759
$ df -Th /
/dev/vda4      ext4    1007G  529G  438G  55% /
$ cat /sys/block/vda/queue/scheduler ; cat /sys/fs/cgroup/io.cost.qos
[none] mq-deadline
(empty)
$ docker info | grep -iE "cgroup|storage"
 Storage Driver: overlayfs
 Cgroup Driver: systemd
 Cgroup Version: 2
$ cat /proc/swaps
/swapfile  file  8388604  6749004  -2
$ systemctl is-active systemd-oomd
inactive
$ zgrep OVERLAY /boot/config-$(uname -r)
CONFIG_OVERLAY_FS=m
# CONFIG_OVERLAY_FS_REDIRECT_DIR is not set
# CONFIG_OVERLAY_FS_INDEX is not set
# CONFIG_OVERLAY_FS_METACOPY is not set
$ ls /sys/fs/cgroup/system.slice/actions.runner.*runner-a1.service/ | grep -E "peak|pressure"
memory.peak memory.swap.peak pids.peak cpu.pressure io.pressure memory.pressure ...
```

---

## 2. Recommended stack

| Concern | Crate (version) | Decision and notes |
|---|---|---|
| Async runtime | `tokio` 1.53 (`full`) | Standard choice. `AsyncFd` with `Interest::PRIORITY` (Linux-only) handles PSI `POLLPRI` without a helper thread |
| HTTP client | `reqwest` 0.13.5, `default-features = false, features = ["json","rustls","stream","http2"]` | Only a few GitHub endpoints are needed, so a thin client beats `octocrab` 0.54, which lacks the scale-set API and pulls in a large surface. `backon` 1.6 for retry/backoff |
| HTTP server | `axum` 0.8.9 + `tower-http` 0.7 | Serves webhooks (if used), `/metrics`, `/healthz`, an admin API, and later the cache server (gha-cache-oxide is axum 0.8 too) |
| Low-level HTTP | `hyper` 1.11 + `hyper-util` 0.1.21 + `http-body-util` 0.1.5 | For the per-job **Docker API proxy** on a unix socket. Needs `hyper::upgrade::on` so `attach`/`exec` hijack streams pass through |
| GitHub App auth | `jsonwebtoken` 11.1 (**v10+ requires picking a crypto backend feature**: `rust_crypto` or `aws_lc_rs`) + `secrecy` 0.10 | RS256 app JWT with `iat-60s` and `exp≤10min`, exchanged for an installation token (1h), cached and refreshed at about 50 min. Webhook HMAC via `hmac` 0.13 + `sha2` 0.11 |
| Job discovery | Port of **`actions/scaleset`** (the official Go client, extracted from ARC 0.14, about 1.5k lines of non-test code) into a small Rust module, **or** `workflow_job` webhooks for v1 | The scale-set message-session long-poll needs no inbound port. GitHub assigns jobs to the scale set and sends JobAssigned/Started/Completed events. The JIT config comes from the scale-set API or from `POST /repos/{o}/{r}/actions/runners/generate-jitconfig` (repo "Administration: write"; org "Self-hosted runners: write") |
| systemd control | `zbus` 5.19 + **`zbus_systemd` 0.26200.0** (feature `systemd1`; the version encodes systemd 262 API) | `ManagerProxy::start_transient_unit(name, mode, Vec<(String, OwnedValue)>, aux)`, `stop_unit`, `kill_unit`, `receive_job_removed`, `subscribe`. Avoid `systemd-zbus` (stale since 2025-05) |
| cgroup v2 reading | **Direct file reads** with small parsers (`cpu.stat`, `memory.stat`, `memory.peak`, `memory.events`, `io.stat`, `*.pressure`, `pids.peak`) | systemd owns the tree, so **never write cgroup files that systemd manages**. `cgroups-rs` 0.5.1 is a writer/manager aimed at Kata and brings no benefit here. `procfs` 0.18 is fine for `/proc/meminfo` and `/proc/pressure` |
| PSI triggers | Hand-rolled: open `/proc/pressure/memory` (or `<slice>/memory.pressure`) with `O_RDWR\|O_NONBLOCK`, write `"some 150000 1000000"`, wrap in `AsyncFd` with `Interest::PRIORITY` | Window 500 ms to 10 s (root). Unprivileged users need windows that are multiples of 2 s. `POLLERR` means the cgroup is gone. The `psi` crate (0.1.2, 2019) is dead |
| Docker control | `bollard` 0.21.1 | List/remove by label, events stream (container→job attribution), stats, networks, image pre-pull. `HostConfig.cgroup_parent` exists, but the runner does not use bollard (see risk 1) |
| Syscalls / mounts | `rustix` 1.1.5 (features `mount`, `fs`, `process`) + `nix` 0.31 where rustix lacks coverage | `rustix::mount::{fsopen, fsconfig_set_string, fsconfig_create, fsmount, move_mount}` builds overlay mounts with `lowerdir+` (no page-size option limit) |
| Reflink (optional backend) | `reflink-copy` 0.1.30 (`FICLONE`) | Only on XFS/btrfs. ext4 has no reflinks |
| State DB | **`sqlx` 0.9 (`sqlite`, `runtime-tokio`, `migrate`)** | **Do not mix with `rusqlite` 0.40.** `rusqlite` 0.40 needs `libsqlite3-sys ^0.38.2`, while `sqlx-sqlite` 0.9 needs `>=0.30.1,<0.38`, and both declare `links = "sqlite3"`, so Cargo refuses the pair. `gha-cache-oxide` already uses sqlx 0.9, so sqlx keeps one SQLite for the eventual in-process cache server. Use WAL and one writer pool connection |
| Metrics | `prometheus-client` 0.25.1 (OpenMetrics, typed families) on `/metrics`. Optional `opentelemetry`/`opentelemetry-otlp`/`opentelemetry_sdk` 0.33 + `tracing-opentelemetry` 0.34 | Per-job final numbers go to SQLite (source of truth). Prometheus exposes live gauges and histograms. Add OTLP traces later |
| Config | `serde` 1.0.229 + `toml` 1.1 (TOML 1.1 spec) + `humantime-serde` + `bytesize` 2.7 + a hand-written `validate()` (or `garde` 0.23) | Skip `figment` (last release 2024-05). Put secrets (the App private key) in files referenced by path, loaded through systemd `LoadCredential=` |
| Logging | `tracing` 0.1.44 + `tracing-subscriber` 0.3.23 (`env-filter`,`json`) + `tracing-journald` 0.3.2 | Spans keyed by `job_id` and `runner_name`. Journald in production, JSON in development |
| Service integration | `sd-notify` 0.5 | `Type=notify` + `WatchdogSec=` for the engine unit |
| Errors / CLI | `thiserror` 2.0.21 (libraries), `anyhow` 1.0.104 (binary), `clap` 4.6 | |
| Misc | `uuid` 1.26, `tokio-util` 0.7.19 (`CancellationToken`, `TaskTracker`), `tokio-stream` 0.1.19 | |
| Cache server (phase 2) | **`mpecan/gha-cache-oxide`** (Rust port of falcondev-oss `github-actions-cache-server`. Cache v2 Twirp + Azure-style block upload, fs/S3 storage, SQLite/Postgres, OIDC JWT verification; axum 0.8, sqlx 0.9, jsonwebtoken 11, object_store 0.14; exposes a `lib` target) | Early development (M1 in progress), but its dependency set matches ours exactly. Plan: run it as a sidecar binary first, then embed it as a library once it stabilizes. The falcondev TS server is the reference for the protocol |

### Cgroup and unit topology

```
-.slice
└─ ci.slice                         (static unit file: MemoryMax=26G, MemoryLow for engine headroom, CPUWeight=100)
   └─ ci-j<jobid>.slice             (transient: MemoryHigh, MemoryMax, MemorySwapMax=0, CPUWeight, TasksMax,
      │                              IOAccounting=yes, optional IOReadBandwidthMax/IOWriteBandwidthMax)
      ├─ ci-j<jobid>-runner.service (transient: ExecStart=a(sasb) run.sh --jitconfig …, User=actions, Slice=ci-j<jobid>.slice,
      │                              KillMode=control-group, RuntimeMaxUSec, CollectMode=inactive-or-failed,
      │                              Environment=DOCKER_HOST=unix:///run/ci/j<jobid>/docker.sock ACTIONS_RESULTS_URL=…)
      └─ docker-<cid>.scope …       (created by dockerd/runc via systemd because CgroupParent=ci-j<jobid>.slice)
system.slice/ci-engine.service      (the daemon itself; Type=notify)
```

- Create the transient **slice** before any container references it. If the slice does not exist when Docker names it, systemd creates it implicitly **with no limits**.
- Teardown order: runner service exits (JobRemoved signal) → `docker rm -f` by label `ci.job=<id>` → remove per-job network → **read the final `cpu.stat`/`memory.peak`/`io.stat`/`*.pressure` from `/sys/fs/cgroup/ci.slice/ci-j<id>.slice/`** → `StopUnit(slice)` → unmount the workdir → persist to SQLite. The cgroup directory disappears once the slice stops.
- `Delegate=` is not needed, because we never create sub-cgroups ourselves (Docker asks systemd for scopes).

---

## 3. How service containers get into the job cgroup

Facts from runner source (`src/Runner.Worker/Container/DockerCommandManager.cs`, `ContainerInfo.cs`, `ContainerActionHandler.cs`):

- The runner shells out to `docker`, which it resolves through `WhichUtil.Which("docker")` on `PATH`. It calls `ExecuteAsync(..., environment: null)`, so **the child inherits the runner's environment, and `DOCKER_HOST` is honored**. Job steps also inherit the runner environment.
- `docker create` options are `--name`, `--label <sha256(.runner)[0:6]>`, `--network`, `--network-alias`, `-p`, then **`container.ContainerCreateOptions`** (the workflow's `services.*.options:` string, which is how `--cgroup-parent` could be set per workflow, but the workflow cannot know the job slice name).
- **Container jobs and Docker actions hard-code `-v /var/run/docker.sock:/var/run/docker.sock`** (`ContainerInfo.cs:59`, `ContainerActionHandler.cs:193`). dockerd resolves that path on the host, so it bypasses any `DOCKER_HOST` redirection.
- `ACTIONS_RUNNER_CONTAINER_HOOKS` (Node script: `prepare_job`, `run_container_step`, `run_script_step`, `cleanup_job`) activates only when the env var is set **and** the server sends the `DistributedTask.AllowRunnerContainerHooks` variable (`FeatureManager.cs`). It replaces only the runner's own job/service/container-action handling. It **does not** see `docker run` inside a step or testcontainers.

| Option | Catches runner services | Catches steps' `docker`/compose | Catches testcontainers (API) | Container-job socket mount | Verdict |
|---|---|---|---|---|---|
| `docker` wrapper on `PATH` that injects `--cgroup-parent` into `create`/`run` | yes | yes (if steps use PATH) | **no** | no | Too narrow. The host already runs testcontainers |
| Container hooks | yes | no | no | n/a (hook decides) | Narrow, and needs Node plus a server flag |
| daemon.json `cgroup-parent` | global only | global | global | global | Not per-job, but a useful **backstop**: set it to `ci-orphans.slice` so nothing lands in `system.slice` unbounded |
| Per-job dockerd (`--containerd-namespace` per job) | yes | yes | yes | yes, if it runs in its own mount namespace with its socket bind-mounted at `/var/run/docker.sock` | Heavy: about 100 MB each, image store per namespace, cold pulls. Kills warm image caching |
| OCI runtime wrapper (custom `runtimes` entry that rewrites `cgroupsPath` from a label) | yes | yes | yes | yes | Works at the lowest level, but dockerd/containerd stats and cgroup bookkeeping assume their own path. Fragile |
| **Per-job Docker API proxy (recommended)** on `/run/ci/j<id>/docker.sock`, `DOCKER_HOST` set on the runner unit | yes | yes | yes | **yes: rewrite the bind `"/var/run/docker.sock:…"` → `"/run/ci/j<id>/docker.sock:/var/run/docker.sock"` in the create body** | Recommended |

The proxy (hyper, one listener per job) forwards everything verbatim except `POST /v*/containers/create`. There it parses the JSON, sets `HostConfig.CgroupParent = "ci-j<id>.slice"`, adds labels `ci.job=<id>`, optionally caps `HostConfig.Memory` and `NanoCpus` to the job budget, and rewrites docker.sock binds. It can also reject `Privileged`, `PidMode=host`, `NetworkMode=host`, and `CgroupnsMode=host` if we want policy. `POST /networks/create` gets the job label too. Upgrades (`attach`, `exec/start`, `/session`, BuildKit `grpc`) must be spliced as raw byte streams after the `101`/hijack. **Remove user `actions` from group `docker`** so the proxy is the only path. Note that raw Docker socket access remains root-equivalent: the proxy adds attribution, not a security boundary.

---

## 4. Cache server and redirection

- The runner sets `ACTIONS_RESULTS_URL` **per step** from the job message's `SystemVssConnection.Data["ResultsServiceUrl"]` (`NodeScriptActionHandler.cs:69-74`, and the same in `ContainerActionHandler.cs:226-239`). It also sets `ACTIONS_CACHE_SERVICE_V2=true` when the server variable `actions_uses_cache_service_v2` is on. A value in `.env` or the unit environment gets **overwritten**.
- Established workaround (falcondev docs, gha-cache-oxide, and several public runner images such as `mitigate-dev/actions-runner`, `PradyumnaKrishna/actions-runner-stack`, `solcreek/firerunner`): **byte-patch the UTF-16LE literal `ACTIONS_RESULTS_URL` → `ACTIONS_RESULTS_ORL` in `bin/Runner.Worker.dll`** (same length). The runner then writes GitHub's URL into a dead variable, and our `ACTIONS_RESULTS_URL` (with a trailing `/`) from the unit environment survives. falcondev also ships a fork image that reads `CUSTOM_ACTIONS_RESULTS_URL`.
- Consequences:
  1. The results URL also carries artifacts v4, job summaries and step telemetry, so the cache server **must proxy every non-cache Twirp path to `https://results-receiver.actions.githubusercontent.com`** (gha-cache-oxide has this catch-all through `DEFAULT_ACTIONS_RESULTS_URL`, capped by `PROXY_MAX_REQUEST_BODY_BYTES`).
  2. The patch must be re-applied for **every runner release**. The engine should own a runner-template pipeline: download the release → verify SHA-256 against the release notes → patch → verify with `strings -el bin/Runner.Worker.dll | grep -c '^ACTIONS_RESULTS_ORL$' == 1` → publish as an immutable template generation. If verification fails, run **unpatched** (fall back to GitHub's cache) and alert.
  3. GitHub deprioritizes runners more than 30 days behind, and JIT runners self-update. Keep templates current with a daily poll of `releases/latest`, so the self-update path never triggers (it would discard the patch).
  4. Tokens: `actions/cache` sends `ACTIONS_RUNTIME_TOKEN` (a JWT). gha-cache-oxide verifies it against GitHub's OIDC JWKS; keep that on.
  5. The server should listen on a host-local address reachable from service containers and job containers too, for example the docker0 bridge IP or a dedicated bridge, not `127.0.0.1`.

---

## 5. Copy-on-write workdirs

**Primary backend: overlayfs (works today on ext4).**

- Layout: `/var/lib/ci/base/<kind>/<gen>/` holds immutable lowers (the runner template generation, a warm repo checkout with `.git` objects, and warm tool caches such as `_tool`, the Go module cache, and the pnpm store). Per job, `/var/lib/ci/jobs/<id>/{upper,work,merged}`.
- Mount with the new mount API: `fsopen("overlay")`, `fsconfig_set_string("lowerdir+", …)` for each layer, `upperdir`, `workdir`, **`redirect_dir=on`** (otherwise renaming a lower directory returns `EXDEV`, which breaks tools that `rename(2)` directories), then `fsmount` + `move_mount`. The mount must live in the **host mount namespace**, because dockerd resolves bind sources itself.
- Runner install per job = overlay of the patched template generation, so O(1) "copy" of about 250 MB of runner plus externals. `_work` lives inside `merged`.
- Updating the warm base: build a new generation directory and switch new jobs to it. Old generations are garbage-collected once their refcount (mounted jobs) reaches 0. **Never mutate a lower while it is mounted**, because overlay behavior is undefined.
- Crash recovery: on startup, parse `/proc/self/mountinfo`, unmount `MNT_DETACH` any `/var/lib/ci/jobs/*/merged` without a live job, and `rm -rf` the uppers.

**Optional backend: reflink directories** (`reflink-copy`, `FICLONE`) on a loop-mounted XFS image (`mkfs.xfs -m reflink=1`, which needs `xfsprogs` installed), or btrfs subvolume snapshots (O(1), needs `btrfs-progs`). Costs: loop-device overhead and a second filesystem to size. Put it behind a `WorkspaceBackend` trait. Only switch if overlay semantics bite (inotify, hardlink identity, EXDEV corner cases).

**Shared, mutable caches** (Turbo remote/local cache, Go build cache) should not live in lowers. Either keep them as a separate shared RW bind (as `TURBO_CACHE_DIR` does today, and accept races) or move them to the phase-2 cache server.

---

## 6. The five hardest technical risks

1. **Attributing every container to its job's cgroup.** Runner services (CLI), step `docker`/compose, testcontainers (raw API; the host already uses them), and container jobs (hard-coded `/var/run/docker.sock` bind) each take a different path.
   *Mitigation:* a per-job Docker API proxy with `DOCKER_HOST`. Rewrite `containers/create` (CgroupParent, labels, docker.sock bind). Handle HTTP upgrade and hijack correctly (test `docker exec -it`, `docker attach`, `docker compose up`, `docker buildx`/BuildKit sessions, and a testcontainers suite). Drop `actions` from the `docker` group. Set daemon `cgroup-parent: ci-orphans.slice` as a backstop. Watch the bollard events stream to alert on any container without `ci.job`.

2. **Redirecting `actions/cache` to a local server.** It needs a binary patch of `Runner.Worker.dll` on every runner release, forced fresh-runner upgrades (30-day rule), and pass-through proxying for artifacts, summaries and telemetry that share `ACTIONS_RESULTS_URL`. If GitHub changes the lookup, the patch silently no-ops.
   *Mitigation:* an engine-owned template pipeline with post-patch verification and fail-open to GitHub's cache. The cache server proxies unknown paths upstream. Integration-test `actions/cache@v4`/`v5` save and restore plus `actions/upload-artifact@v4` on each template promotion. Track upstream for a first-class override.

3. **Admission control that is honest on a noisy, swapping VPS.** System CPU PSI already sits at ~48% `some` with the current fleet. 6.4 GiB of swap is in use, so memory PSI rises late and then falls off a cliff. `MemoryHigh` throttles jobs into long stalls instead of OOMing. `IOWeight` does nothing because the scheduler is `none` and iocost is disabled.
   *Mitigation:* admit on **reservations first** (per-job-class `MemoryHigh` sum ≤ budget, a CPU-slot budget) and use **PSI as a brake**: cgroup-level `memory.pressure` triggers on `ci.slice` pause admission, with hysteresis over `avg10`/`avg60`. Set `MemorySwapMax=0` on job slices. Enable iocost (`io.cost.qos enable=1`) or use `io.max` caps. Record `memory.events` (`high`, `max`, `oom_kill`) per job to tune classes. Migrate the 10 persistent runners into `ci.slice` gradually so the engine sees the whole load.

4. **Lifecycle correctness across GitHub, systemd, Docker and mounts.** A JIT runner may take *any* job that matches its labels. JIT configs can be orphaned (registered, never used). The engine may restart mid-job, and transient units survive it. Mounts leak on crash. JobRemoved and unit-state races occur.
   *Mitigation:* use the runner scale-set API (GitHub assigns jobs to the scale set and reports JobStarted/Completed with runner names). Use deterministic unit names (`ci-j<runnerId>.slice`). Run a **reconcile loop** that treats systemd (`ListUnitsByPatterns ci-j*`), Docker (label `ci.job`), mountinfo, and GitHub runners (`GET /actions/runners`) as ground truth and SQLite as the journal. Make every step idempotent. Delete offline JIT runners past a TTL. Use `RuntimeMaxUSec` as a hard ceiling.

5. **CoW workspace semantics and warm-cache freshness.** Overlay on ext4 without `redirect_dir` gives `EXDEV` on directory renames. Bind-mounting `merged` into containers works but runs into UID and ownership mismatches with the job's `actions` user. Updating lowers safely requires generations and refcounts. Disk usage from uppers grows without bound on long jobs.
   *Mitigation:* mount with `redirect_dir=on`. Use generation directories with refcounted GC. Keep a per-job disk quota (ext4 has no per-dir quota without project quotas, so enforce a soft limit by polling `du` of the upper, or move to an XFS loop with project quotas). Build a test matrix of `git clone`/`checkout`, `pnpm install`, `go build`, `cargo build`, and `docker build` with context in `merged`. Keep the reflink backend as a fallback.

(Noted but not in the top five: no KVM, so no microVM isolation. Every job shares the kernel, and Docker socket access is root-equivalent. Treat the host as single-tenant and trusted-repos-only.)

---

## 7. Proposed workspace layout

```
ci-engine/
├─ Cargo.toml                    # [workspace] resolver = "3", shared [workspace.dependencies]
├─ crates/
│  ├─ engine/          (bin)     # main.rs: config load, tracing init, sd-notify, task wiring, signal handling
│  │   └─ src/{main.rs, supervisor.rs, reconcile.rs, lifecycle.rs}
│  ├─ config/                    # serde+toml types, validate(), secret-file loading
│  ├─ github/                    # app_jwt.rs (jsonwebtoken), installation_token.rs (cache+refresh),
│  │                             # rest.rs (jitconfig, runners list/delete, releases), scaleset/{session,messages,types}.rs,
│  │                             # webhook.rs (HMAC verify, workflow_job) — reqwest only
│  ├─ systemd/                   # zbus_systemd wrappers: SliceSpec/ServiceSpec → Vec<(String, OwnedValue)>,
│  │                             # start/stop/kill, wait_job_removed, list_units(pattern), property helpers
│  ├─ cgroup/                    # pure parsers (cpu.stat, memory.stat/peak/events, io.stat, pressure) + JobCgroup reader
│  │                             # psi.rs: PsiTrigger (AsyncFd<OwnedFd>, Interest::PRIORITY) → Stream<PressureEvent>
│  ├─ admission/                 # pure policy: job classes, reservations, PSI brake with hysteresis (no I/O → unit-testable)
│  ├─ docker/                    # control.rs (bollard: cleanup-by-label, events, prepull),
│  │                             # proxy/{listener,rewrite,upgrade}.rs (hyper unix-socket proxy per job)
│  ├─ workspace/                 # trait WorkspaceBackend { prepare, teardown, gc }; overlay.rs (rustix new mount API),
│  │                             # reflink.rs (reflink-copy), generations.rs (base gens + refcounts), recover.rs (mountinfo)
│  ├─ runner/                    # template.rs (download, sha256, UTF-16 patch + verify, promote gen), launch.rs (env, argv)
│  ├─ store/                     # sqlx sqlite: migrations/, jobs, runners, samples, template_gens repositories
│  ├─ metrics/                   # prometheus-client registry, axum /metrics, per-job summary export
│  └─ cache/           (phase 2) # thin adapter embedding gha-cache-oxide (lib) or sidecar supervision
├─ deploy/                       # ci-engine.service, ci.slice, ci-orphans.slice, daemon.json snippet, tmpfiles.d
└─ tests/                        # integration: runs on the VPS (or a Debian 13 VM) behind a feature flag
```

Module boundaries follow the "deep module" rule: `admission` and the `cgroup` parsers are pure and I/O-free. `systemd`, `docker`, and `workspace` each hide one OS subsystem behind a small interface, so `engine::lifecycle` reads as the job state machine: `Queued → Admitted → Prepared(ws, slice, proxy) → Running → Draining → Measured → Cleaned`.

Minimal `[workspace.dependencies]` pin set:

```toml
tokio = { version = "1.53", features = ["full"] }
tokio-util = { version = "0.7.19", features = ["rt"] }
reqwest = { version = "0.13.5", default-features = false, features = ["json", "rustls", "stream", "http2"] }
axum = "0.8.9"
hyper = { version = "1.11", features = ["http1", "server", "client"] }
hyper-util = { version = "0.1.21", features = ["tokio"] }
http-body-util = "0.1.5"
jsonwebtoken = { version = "11.1", features = ["rust_crypto"] }
hmac = "0.13"
sha2 = "0.11"
secrecy = "0.10"
zbus = { version = "5.19", default-features = false, features = ["tokio"] }
zbus_systemd = { version = "0.26200", features = ["systemd1"] }
bollard = "0.21.1"
rustix = { version = "1.1.5", features = ["mount", "fs", "process"] }
reflink-copy = "0.1.30"
sqlx = { version = "0.9", default-features = false, features = ["runtime-tokio", "sqlite", "migrate", "macros"] }
prometheus-client = "0.25.1"
tracing = "0.1.44"
tracing-subscriber = { version = "0.3.23", features = ["env-filter", "json"] }
tracing-journald = "0.3.2"
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1"
toml = "1.1"
humantime-serde = "1.1"
bytesize = { version = "2.7", features = ["serde"] }
sd-notify = "0.5"
backon = "1.6"
thiserror = "2.0.21"
anyhow = "1.0.104"
clap = { version = "4.6", features = ["derive"] }
uuid = { version = "1.26", features = ["v4"] }
```

(Check the `zbus` tokio feature and the `zbus_systemd` feature names against docs.rs when scaffolding. `zbus_systemd` re-exports `zbus`, so match its major version.)

---

## Sources

- actions/runner source: `src/Runner.Worker/Container/DockerCommandManager.cs`, `ContainerInfo.cs`, `Handlers/ContainerActionHandler.cs`, `Handlers/NodeScriptActionHandler.cs`, `FeatureManager.cs`, `Runner.Listener/Runner.cs` (github.com/actions/runner @ ca43437)
- https://github.com/actions/scaleset and https://github.blog/changelog/2026-03-19-actions-runner-controller-release-0-14-0/
- https://docs.github.com/en/rest/actions/self-hosted-runners (generate-jitconfig)
- https://github.com/mpecan/gha-cache-oxide (README, Cargo.toml)
- https://github.com/falcondev-oss/github-actions-cache-server and https://gha-cache-server.falcondev.io/getting-started/
- GitHub code search `ACTIONS_RESULTS_ORL` (mitigate-dev/actions-runner, PradyumnaKrishna/actions-runner-stack, solcreek/firerunner, navruzm/gha-cache-server docs)
- https://docs.kernel.org/accounting/psi.html
- https://docs.rs/zbus_systemd/latest/zbus_systemd/systemd1/struct.ManagerProxy.html
- https://docs.rs/tokio/latest/tokio/io/struct.Interest.html
- crates.io API (versions and dependency ranges, 2026-09-30)
