# 0003: Admission by reservations plus PSI brake

- Status: Accepted
- Date: 2026-09-30

## Context and problem statement

Momentary utilization can look low before a newly started job consumes memory or CPU. Pressure can rise late and oscillate on busy hosts.

## Considered options

- Admit only when reserved CPU and RAM fit the node budget, retaining OS headroom.
- PSI-only admission; fixed runner counts; admission from instantaneous free RAM.

## Decision outcome

Admit only when reserved CPU and RAM fit the node budget, retaining OS headroom. Use PSI to pause admission with an upper threshold and resume after recovery below a lower threshold. Set job MemorySwapMax=0.

## Consequences

Reservations bound promised capacity, and hysteresis reduces churn. PSI cannot grant capacity beyond reservations. Job classes need measurement-driven tuning; conservative estimates may leave resources unused.

See [architecture](../architecture.md) for the target design. M0 contains API
skeletons; this decision does not imply implemented execution behavior.
