# CI time report

1 successful first-attempt runs; 1 jobs in the window. Clear infra: **0.00%**; confirmed flaky: **0.00%** (1 executed non-aggregator jobs). Combined: **0.00%**; target <1%: **below target on observed evidence**.

Window: 2026-01-01T00:00:00Z to 2026-01-01T00:05:01Z (exclusive). Markdown durations are minutes.

## End-to-end and achievable floor

Successful first attempts, run created → last executed job completed. Best case removes queue and in-job waits and uses low-concurrency execution factors. Half-baseline columns are the 2× improvement targets for this input window.

| Repo | Event | Runs | E2E p50 | p90 | p99 | Floor p50 | p90 | Half p50 | Half p90 | Paths needs / inferred |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| acme/app | pull_request | 1 | 5.00 | 5.00 | 5.00 | 4.83 | 4.83 | 2.50 | 2.50 | 0 / 1 |

## Critical-path composition

Means within E2E percentile bands; work excludes matching in-job wait steps.

| Repo / event | Band | Runs | E2E | Queue | Work | Dispatch gap | In-job wait |
|---|---|---:|---:|---:|---:|---:|---:|
| acme/app / pull_request | &lt;=p50 | 1 | 5.00 | 0.17 | 4.67 | 0.17 | 0.00 |
| acme/app / pull_request | p40-p60 | 1 | 5.00 | 0.17 | 4.67 | 0.17 | 0.00 |
| acme/app / pull_request | p85-p95 | 1 | 5.00 | 0.17 | 4.67 | 0.17 | 0.00 |
| acme/app / pull_request | &gt;=p90 | 1 | 5.00 | 0.17 | 4.67 | 0.17 | 0.00 |

## Per-job timing

Fail/cancel rates exclude skipped jobs; duration and runner-minutes use jobs that ran.

| Repo / workflow | Job | Count / ran | Duration p50 / p90 | Queue p50 / p90 | Fail % | Cancel % | Runner-min |
|---|---|---:|---:|---:|---:|---:|---:|
| acme/app / CI | tests | 1 / 1 | 4.67 / 4.67 | 0.17 / 0.17 | 0.00 | 0.00 | 4.7 |

## Host concurrency

Peak: **1**; idle: **7.0%**; busy: **0.08 hours**; clipped same-runner overlaps: 0. Scope: all observed runners.

| Concurrent jobs | Hours | Wall % | Busy % |
|---:|---:|---:|---:|
| 0 | 0.01 | 7.0 | 0.0 |
| 1 | 0.08 | 93.0 | 100.0 |

| Runner label | Capacity | Source | Peak | Saturation hours |
|---|---:|---|---:|---:|
| host-a | 1 | observed runner count | 1 | 0.08 |
| linux | 1 | observed runner count | 1 | 0.08 |

## Contention

Successful job durations exclude matching in-job waits. Samples are grouped by each job's time-weighted host concurrency: low 1–3, high ≥7. Steps use their parent job's concurrency band.

| Repo | Job / step | Low n | Low p50 | High n | High p50 | High / low |
|---|---|---:|---:|---:|---:|---:|
| acme/app | tests | 1 | 4.67 | 0 | — | — |
| acme/app | tests / Run tests | 1 | 4.67 | 0 | — | — |

## Failure classification

| Class | Jobs |
|---|---:|
| Clear infra | 0 |
| Confirmed flaky | 0 |
| Code | 0 |
| Superseded | 0 |
| Unknown | 0 |
| Cancelled before start (excluded) | 0 |
| Aggregators (excluded, all conclusions) | 0 |

Unknown cancellations and missing evidence require review; the observed rate alone does not certify the target. Flaky rerun evidence does not by itself establish an infrastructure cause.

## Method and limitations

- Best-case estimates remove queue and matching wait steps, retain dispatch gaps, and scale each job by min(1, low-concurrency median / overall median). They use the observed graph, not a hypothetical workflow redesign.
- Timestamp-inferred critical paths are approximate (3-second dispatch tolerance); matrix limits and reusable workflows are not reconstructed.
