# Per-job Docker attribution

The node owns one in-process Unix HTTP proxy per job, at
`<run_dir>/jobs/<id>/docker.sock`. The socket is mode 0660, with the runner's
UID/GID. Its directories remain daemon-owned and traversable (0711). JIT
credential files sharing `/run/runwell` remain root-only (0600). The limited
`ci-rw-j<id>.slice` exists before the socket opens. Production standalone wiring
uses `ProxyManager`; systemd-only node probes can omit Docker setup.

The runner receives `DOCKER_HOST=unix://<socket>` and
`TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE=<socket>`. No
`TESTCONTAINERS_HOST_OVERRIDE` is set: the upstream daemon and runner are on the
same Linux host, so normal testcontainers host/port discovery applies. An
application running inside another container may need its own reachable host
address; forcing one globally would break those clients.

## Rewrites

All routes below match both unversioned and `/v1.<digits>` paths.

| POST route | Change |
| --- | --- |
| `/containers/create` | Force `HostConfig.CgroupParent`; merge `io.runwell.job` and `io.runwell.node`; remap Docker socket bind sources; cap missing/zero/excessive `Memory` at slice MemoryMax by default. Other limits stay intact. |
| `/build` | Replace all `cgroupparent` query values and merge JSON `labels`; leave context streaming. Other query segments retain their original encoding. |
| `/networks/create`, `/volumes/create` | Merge the two ownership labels. |
| `/containers/{id}/update` | Reject `CgroupParent`, including case aliases; preserve other update bodies exactly. |
| All other requests | Preserve method, URI, end-to-end headers and payload bytes. |

Go-compatible case aliases cannot override forced JSON fields. `/info` is read
once per manager (after API version negotiation), and its driver selects either
the slice unit name for systemd or `/ci.slice/ci-rw.slice/ci-rw-j<id>.slice` for
cgroupfs. Unknown drivers fail closed. Target hosts must use the systemd driver;
only that driver has a privileged attribution test.

HTTP is parsed and serialized by Hyper, so transport chunk boundaries, header
casing/order, and HTTP framing are not promised to remain wire-identical. Body
bytes, trailers and opaque upgraded protocol bytes remain intact. JSON errors
return Docker-style `message` objects: 413 for the configurable size cap, 400
for invalid rewrite data or cgroup updates, and 403 for optional host-access
policy rejection. Tar requests are never collected in memory.

## Configuration

```toml
[standalone.docker_proxy]
upstream_socket = "/var/run/docker.sock"
run_dir = "/run/runwell"
max_json_bytes = 2097152
cap_memory = true
deny_host_access = false
stop_seconds = 10
```

The optional host-access policy refuses privileged containers and host PID or
network namespaces. Their use is logged even with the policy disabled. This
policy does not isolate trusted workflows; see [SECURITY.md](../../SECURITY.md).
With a custom upstream path, keep that socket inaccessible to runner accounts.

## Lifecycle and measurements

Preparation starts the proxy after creating the slice and before JIT launch.
Reconcile restores proxies for surviving runners. Inventory combines units,
runner directories, runtime directories, and Docker object labels, so label-only
orphans reach M3's existing remote-registration deletion gate.

After the runner stops, the proxy refuses new requests, waits up to 30 seconds
for accepted JSON mutations to receive daemon response headers (even after a
client disconnect), and joins all HTTP/upgrade tasks. A drain timeout retains
durable cleanup work for retry rather than claiming successful deletion.
Bollard contacts the upstream directly: stop containers with the grace timeout,
force-remove containers without removing unlabeled anonymous volumes, then
remove labeled networks and volumes. Both job and node labels are checked
locally as well as in daemon filters. Missing objects are success; other failures
retain the controller's durable cleanup work. Images/build caches are shared
and are not removed. Docker-created implicit/anonymous volumes without these
labels are deliberately not removed.

Docker cleanup runs in `stop_runner`, before `measure`, retaining the slice until M3 journals its
final counters. `linux_dockerproxy::docker_cli_attribution_exec_socket_build_and_label_scoped_teardown`
asserts `/proc/<container-pid>/cgroup` is below the job slice and that the parent
CPU and memory peak counters increase and survive removal of the containers.
That ignored test must pass on Linux before treating the accounting assumption
as experimentally verified; macOS fake-daemon tests cannot establish it.

A daemon restart interrupts active streams. The proxy reopens the same pathname,
but an existing container's bind-mounted socket still refers to the old inode;
that container must be recreated to reconnect. The in-process proxy does not
promise uninterrupted Docker access while the node is stopped.

## BuildKit scope

Classic and Engine HTTP `/build?version=2` calls get query attribution and labels.
Modern Buildx can submit solves over upgraded `/grpc`; the proxy intentionally
splices that opaque protocol. It cannot inject labels or cgroup settings inside
those protobuf messages. Furthermore, the
[Moby v28 BuildKit adapter](https://github.com/moby/moby/blob/v28.0.0/builder/builder-next/builder.go)
forwards build labels but does not apply the HTTP build option's cgroup parent to
its worker. Successful BuildKit builds therefore prove streaming compatibility,
**not complete BuildKit CPU/memory attribution**. Shared daemon worker resources
and image/cache work can remain outside the job slice. Full attribution needs a
separate BuildKit worker per job or a protocol-aware BuildKit integration; that
would exceed transparent Docker HTTP rewriting.

## Verification

- `tests/rewrite.rs`: route matching; container/network/volume golden JSON;
  build query golden; driver formats; binds/mounts; limits; cap; policy; updates.
- `tests/transport.rs`: Unix socket ownership/mode, echoed rewrites and exact
  passthrough payloads; fixed/chunked oversized JSON; streaming logs, events,
  stats, pull and build responses delivered before EOF, with trailers;
  `tcp` attach/exec and `h2c` session/grpc upgrades, binary TTY/multiplexed bytes,
  bidirectional stdin and output after stdin half-close.
- `tests/large_upload.rs`: generated 200 MiB tar padding through build and archive
  routes; a counter asserts fewer than 8 MiB outstanding across producer, proxy,
  kernel sockets and upstream. This proves bounded payload buffering without
  making an allocator-dependent claim about total process RSS.
- `tests/cleanup.rs`: cached driver detection, stale-socket recovery, shutdown of
  open streams, label-only inventory, exact label checks, ordered/idempotent
  deletion and protection of unlabeled/other-node/other-job objects.
- `runwell-node/tests/linux_dockerproxy.rs`: ignored real Docker/systemd test,
  included by the existing root `linux-privileged` CI command for runwell-node.
  Covers the real unprivileged runner environment/socket access, cgroup
  membership/counters, CLI stdin, both bind forms, BuildKit build
  compatibility, cleanup and an unlabeled control container.
