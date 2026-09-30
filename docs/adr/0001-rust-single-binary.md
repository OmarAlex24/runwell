# 0001: Rust single binary

- Status: Accepted
- Date: 2026-09-30

## Context and problem statement

Owned Linux hosts need a small deployable control plane, while development and analysis should work on macOS.

## Considered options

- Use one Rust binary with controller, node, report, simulate, advise, and version modes.
- Separate service binaries; a runtime-heavy scripting stack.

## Decision outcome

Use one Rust binary with controller, node, report, simulate, advise, and version modes. Use edition 2024, resolver 3, and focused workspace crates with explicit APIs.

## Consequences

A single distribution artifact reduces deployment overhead. Rust supports typed lifecycle boundaries and safe subsystem wrappers. Linux operations remain target-gated; the upstream runner is an external executable, not embedded in runwell.

See [architecture](../architecture.md) for the target design. M0 contains API
skeletons; this decision does not imply implemented execution behavior.
