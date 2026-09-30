# 0002: Port the scale-set protocol instead of webhooks

- Status: Accepted
- Date: 2026-09-30

## Context and problem statement

The controller needs demand, acquisition, and runner assignment signals without operating a public inbound webhook endpoint.

## Considered options

- Port the scale-set HTTP protocol from the reference research into runwell-scaleset.
- workflow_job webhooks plus REST polling; persistent runner inventory alone.

## Decision outcome

Port the scale-set HTTP protocol from the reference research into runwell-scaleset. Own one session per class, handle idempotently, then acknowledge; distinguish admin and queue tokens.

## Consequences

Outbound long polling avoids webhook delivery plumbing and exposes demand statistics. The preview protocol can change and requires fixtures and contract tests. Acquisition selects requests, but GitHub decides the matching idle runner; do not promise exact request placement.

See [architecture](../architecture.md) for the target design. M0 contains API
skeletons; this decision does not imply implemented execution behavior.
