# 0005: Controller/node topology over mTLS

- Status: Accepted
- Date: 2026-09-30

## Context and problem statement

One scheduling view should spread work across a few hosts without confusing local resource ownership or trusting unauthenticated commands.

## Considered options

- Keep policy, GitHub sessions, releases, and SQLite state in the controller.
- A standalone independent daemon per host; unauthenticated local or remote RPC.

## Decision outcome

Keep policy, GitHub sessions, releases, and SQLite state in the controller. Run one node per host for admission and execution. Authenticate both ends of controller-to-node transport using mTLS and stable node identities.

## Consequences

The same topology scales from one host to a small fleet. Transport and reservations must tolerate retries and stale snapshots. Certificate distribution, rotation, reconnect, and durable reconciliation become operational responsibilities.

See [architecture](../architecture.md) for the target design. M0 contains API
skeletons; this decision does not imply implemented execution behavior.

## M5b addendum: HTTP/2 JSON RPC, October 2026

Use HTTP/2 with serde JSON envelopes over rustls, restricted to TLS 1.3 and h2
ALPN. The existing Hyper/Tokio stack provides pooled multiplexed connections and
streaming without protobuf code generation, protoc, or another service runtime.
For a small fleet, explicit Rust request/response enums and a versioned `/v1`
path are sufficient. Incompatible wire changes require a new path/version;
unknown methods fail closed. Four-MiB body limits, bounded handshakes, connection
limits, RPC deadlines and bounded exponential reconnect protect the service.

Both sides validate the private CA and peer role/identity. A single URI SAN
`urn:runwell:node:<id>` or `urn:runwell:controller:<id>` is authoritative; CN is
informational. Server DNS SANs are `<id>.<role>.runwell`, independent of the
configured TCP host:port. The client checks both that DNS SAN and the expected
URI SAN. The controller accepts only configured node IDs, and nodes accept only
the configured controller ID. A CA-signed certificate does not confer fleet
membership. No plaintext or server-auth-only fallback exists.

`POST /v1/rpc` exposes register/report, drain, admit, prepare, workspace,
bind, start, inspect, stop, measure, harvest, cleanup and controller-authorized
orphan cleanup. `GET /v1/events` emits NDJSON full state snapshots over a live
HTTP/2 stream; reconnect recovers current state without depending on missed
intermediate events. Nodes also periodically register/report to the controller.
Controller reconciliation polls full snapshots through the same transport port.

Commands use `(job_id, attempt)`; retries allocate a new durable job ID. Placement
is written before admission and never moves after an ambiguous response. Node
SQLite is a local execution spool, not an independent policy/GitHub database:
it stores reservations, immutable plans, stages, final measurements, report
sequence and cleanup tombstones. Global jobs, GitHub identities, placements,
release selection and failure outbox live in controller SQLite. Neither journal
stores JIT credentials. Tombstones prevent delayed start/prepare messages from
resurrecting a cleaned attempt. A durable start intent is never relaunched after
an uncertain exit. A controller JIT claim allows only one POST; an ambiguous
outcome is resolved by name lookup and DELETE, not another POST.

Disconnected nodes keep services, mounts and reservations. Expired controller
observations fence the attempt and emit an idempotent failure event. DELETE
agent must succeed (or prove absence) before destructive host cleanup; a busy
response defers cleanup. Partition cleanup therefore cannot be guaranteed until
connectivity returns. Do not retry elsewhere before the M5a classifier/retry
policy decides how to handle that uncertainty. Outbox delivery is at least once
across a crash; consumers must deduplicate `(job_id, attempt)` to achieve one
logical classification/retry. Ordinary callback redelivery after acknowledgement
is suppressed durably.

ECDSA P-256 keys/certificates are generated offline with rcgen. Rotation issues a
new generation with the same SAN identity; old and new leaves are accepted while
valid. Trust bundles may contain both CAs during a CA rollover. Daemons reload
TLS material on restart; drain and restart nodes, then explicitly resume. Never
distribute the CA private key. See [operations](../operations.md).
