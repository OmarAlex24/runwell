# 0004: Per-job Docker API proxy for cgroup attribution

- Status: Accepted
- Date: 2026-09-30

## Context and problem statement

Runner service containers, workflow Docker commands, and test libraries use different Docker API paths. All must be charged to the job cgroup.

## Considered options

- Expose a per-job Docker API proxy through DOCKER_HOST.
- CLI wrappers; runner container hooks; a separate Docker daemon per job.

## Decision outcome

Expose a per-job Docker API proxy through DOCKER_HOST. Force container cgroup parents and job labels, label networks, rewrite socket bind sources, and preserve upgraded streams. Create the limited slice first.

## Consequences

One API boundary covers CLI and direct API clients while retaining shared image caches. Upgrade streams and bind rewriting need integration tests. Docker remains root-equivalent: the proxy attributes rather than isolates. Remove direct socket access from runner accounts.

See [architecture](../architecture.md) for the target design. M0 contains API
skeletons; this decision does not imply implemented execution behavior.
