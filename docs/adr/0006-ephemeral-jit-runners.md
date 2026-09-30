# 0006: Ephemeral JIT runners, never persistent

- Status: Accepted
- Date: 2026-09-30

## Context and problem statement

Persistent registrations hold capacity claims and retain workspace state. JIT generation registers an agent before the listener starts and can leave orphans after crashes.

## Considered options

- Reserve capacity before creating a JIT runner.
- Persistent runner pools; shared writable install roots; run.sh lifecycle wrappers.

## Decision outcome

Reserve capacity before creating a JIT runner. Use one independent install root per runner from a SHA-256-verified template, spawn bin/Runner.Listener directly with JIT data in the environment, run one job, and always DELETE agent afterward.

## Consequences

Ephemeral runners reduce cross-job state and enable precise accounting. Persist names before generation and IDs before spawn, use DELETE-first idle removal, and defer when the service reports a busy runner. Template releases must stay within the 30-day update window. Test failures are never retried automatically.

See [architecture](../architecture.md) for the target design. M0 contains API
skeletons; this decision does not imply implemented execution behavior.
