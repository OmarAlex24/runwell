# Controller and node operations

Use trusted workloads on owned Linux x86-64 hosts with systemd, cgroup v2 and
Docker. macOS can run the controller and deterministic tests, not host execution.
Keep the controller database and each node's execution spool on durable local
disks. Back up controller SQLite and its WAL consistently. Never restore an old
node spool over a live host: its report sequence and cleanup tombstones are
fencing data. Each daemon holds an exclusive OS lock; run only one controller
for a database and one node per host.

## Certificates

On an offline administration machine:

```sh
runwell certs ca --out ./offline-ca
runwell certs issue --ca ./offline-ca --out ./controller-1-v1 --role controller --id controller-1
runwell certs issue --ca ./offline-ca --out ./node-1-v1 --role node --id node-1
runwell certs issue --ca ./offline-ca --out ./node-2-v1 --role node --id node-2
```

All generated files use mode 0600 and their directory uses 0700. The default CA
validity is ten years; leaves default to 90 days (`--days` overrides). Existing
files are refused. No command prints keys. Distribute only the matching peer's
`key.pem`, `identity.pem` and public `ca.pem` using an authenticated administrative
channel. Keep the CA key offline. Protect controller GitHub credentials separately
with systemd credentials. Nodes never need the administrative GitHub credential;
the schema's GitHub reference is not read in network node mode.

Set `[transport]` file paths per host. The certificate contains a DNS SAN derived
from its identity and a role-specific URI SAN; the TCP address can be an IP or a
real hostname. Configure the same `network.controller_id` everywhere and list
each allowed ID in `[[network.nodes]]`. Firewall the command/report ports to
fleet peers. A certificate for another controller ID, an unknown node, or the
wrong role is rejected even when signed by the same CA.

For leaf rotation, run `certs issue` again into a fresh directory with the same
role/ID. Stage and verify the new files, atomically switch the configured paths,
and restart the daemon. Both generations work during validity overlap. To rotate
the CA, first distribute a bundle containing the old and new public CAs to all
peers; then rotate leaves and restart, finally remove the old CA after every
peer has switched. This implementation does not distribute CRLs or perform
online revocation: remove an identity from configuration and restart peers to
revoke its membership, or rotate the CA after key compromise.

## Add a node

Copy `examples/runwell.toml` into controller and per-node configurations. Set
local paths, CPU/RAM budgets, `[node].id`, TLS files, and reachable host:port
addresses. `[standalone]` supplies execution settings in both modes. Create the
dedicated runner account and protect its direct access to Docker's host socket
as described in SECURITY.md. Nodes admit authoritatively using their own budget,
max-job limit and PSI hysteresis; controller snapshots never overrule refusal.

Start the controller with `runwell controller --config controller.toml` and each
node with `runwell node --config node.toml`. The controller owns sessions, release
selection, placement and GitHub runner lifecycle. Nodes prepare verified immutable
runner templates selected by the controller. Network mode uses cold workspaces
unless explicit per-class repository routing provides safe warm cache seeds;
GitHub-authorized cache promotion remains disabled on credential-free nodes.
The existing `runwell node --standalone --config runwell.toml` continues to use
an in-process controller and local NodeBackend.

## Drain and upgrade

```sh
runwell node drain --config node.toml
# Wait until reports show no active jobs, then replace/restart the binary.
runwell node resume --config node.toml
```

Drain is durable. Nodes advertise zero free slots and reject new reservations;
accepted work keeps running. SIGTERM also drains. A second signal or drain timeout
exits the daemon while preserving systemd runner services, mounts and reservations.
On restart the node rebuilds admission from its spool and recovers per-job Docker
proxies. It remains drained until `resume`. Restarting the controller closes its
sessions and later re-adopts durable runner identities before opening new ones.

A daemon crash can interrupt in-process Docker proxy connections, even while the
underlying runner service survives. A real host reboot can terminate services and
containers. The controller retains cleanup work, classifies missing execution as
infrastructure evidence, and reconciles surviving disk/Docker state on reconnect.
Do not delete execution journals or manually remove live units during an upgrade.

## Deadlines and failures

`rpc_seconds` bounds each attempt; `rpc_attempts` bounds retries with exponential
backoff. Connections are reused and rebuilt after disconnect. `report_seconds`
controls registration reports. `lost_seconds` fences an unreachable node.
`preparation_seconds` separately bounds admission and template/workspace setup.
Execution timing starts at the durable Start intent/acknowledgement, excluding
preparation, and survives both controller and node restarts. The duration watchdog
uses `expected_seconds * watchdog_multiple`; M5a can supply learned estimates.

`heartbeat_seconds` bounds missing **runner** renewal evidence. The Linux backend
reads successful job-renewal timestamps from bounded tails of the two newest
`_diag/Runner_*.log` files, following the upstream
[JobDispatcher renewal trace](https://github.com/actions/runner/blob/main/src/Runner.Listener/JobDispatcher.cs).
A live process, fresh node reports, file modification times and unrelated log
messages do not refresh it. The last observed renewal is persisted monotonically;
before the first renewal, the durable start time supplies the grace period.
The example allows 180 seconds. Keep this above the runner renewal interval plus
network/reporting jitter, and validate the trace format when changing runner
versions. Node timestamps are translated using report ages, so matching wall-clock
offsets are not required.

The server admits at most 32 live RPC handlers/event streams across all
connections. It rejects overload before reading bodies, retains a permit for
accepted operations after disconnect, and coalesces identical in-flight lifecycle
requests. Overload is retryable and is not classified as a job failure. Cleaned
node leases retain compact idempotency tombstones; reports and job operations
read only indexed active records or a single attempt. Draining deadlines and
signals remain effective during inventory failure and registration retries.

A failure event has a durable `(job_id, attempt)` key. `Hooks::failure` is the M5a
classification/retry port and must be idempotent across controller crashes;
`Hooks::report` is the bounded metrics port, and `Hooks::completed` supplies
terminal measurements for learned durations and job metrics. `LogHooks` only reports evidence and
does not automatically re-run workflows. DELETE agent returning busy always
preserves execution. After a partition, stop, measurement and cleanup remain
pending until the node reconnects; local cleanup and GitHub removal are both
mandatory. Job completion is authoritative only from GitHub events, never exit 0.

## Verification

`crates/runwell-controller/tests/chaos.rs` uses fake transport, host resources,
GitHub and clock. It covers controller/node kill, host loss, Docker outage,
partition, OOM, full disk, duplicate/out-of-order delivery, stale reports,
ambiguous JIT/start, drain and watchdog. Each scenario checks completion or one
logical infrastructure report and eventual absence of fake units, mounts,
containers and GitHub registrations. The real-network smoke test is explicit:

```sh
cargo test -p runwell-controller --test network -- --ignored
```

It runs two localhost nodes with real mTLS, executes two jobs, streams states,
checks reconnect with a reused client, old/new certificate overlap, and rejects
both a CA-signed unauthorized peer and a foreign CA.
A real two-host run is still required for systemd persistence across daemon
upgrades, proxy reconnect effects, real Docker restarts, overlay cleanup under
ENOSPC/OOM, firewall partitions and actual GitHub session/runner reconciliation.
