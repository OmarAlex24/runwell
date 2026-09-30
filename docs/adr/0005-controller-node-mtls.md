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
