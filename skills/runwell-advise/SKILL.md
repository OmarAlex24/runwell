---
name: runwell-advise
description: Use when CI is slow, to optimize GitHub Actions, investigate flaky CI on self-hosted runners, or review workflows.
---

Optimize workflows with a measured baseline and one rule per PR.

1. Obtain the baseline with `runwell report --repo <owner/name> --since 14d --export-trace /tmp/trace.jsonl` using a `GH_TOKEN` or authenticated `gh` session. If authentication or history is unavailable, continue with static analysis and state that timing evidence is unavailable.
2. Run `runwell advise --trace /tmp/trace.jsonl --format json`. For static analysis, omit `--trace`. Rank findings by `estimatedSavings.p50`, then p90; treat estimates as conditional and overlapping, not additive. Review the structured evidence and source locations before selecting a rule. Use `--self-hosted yes|no` when automatic label inference cannot classify dynamic runner expressions.
3. Apply one rule per PR: `runwell advise --fix --rule <id> --trace /tmp/trace.jsonl`. Review every printed unified diff and refusal. For manual findings, hand-edit following the suggested snippet, preserving the existing command flags, dependencies, artifacts, and coverage aggregation. A snippet is a starting point, not a complete replacement workflow. Keep test semantics identical: never delete tests, skip steps, or reduce coverage. Preserve all existing setup and required status checks when moving or sharding work.
4. Validate changed YAML with `actionlint` when installed and run the affected checks. Review the diff for unintended changes. Open a PR describing the selected rule, its evidence, the baseline p50/p90, and conditional expected savings. Static-only PRs must say savings are unmeasured. Exit code 1 means warning findings remain; 2 means invalid input or an operation failed.
5. After merge and a comparable observation window, rerun `runwell report` with the same repository, event population, and time-window length. State measured before/after p50/p90, sample counts, failure rates, and any changes in capacity or workload that confound the comparison. Complete when measured effects are recorded, or clearly state that follow-up measurement is pending.

Guardrails: leave deploy/release gates, secrets, and permissions for human review. Stop and ask when a proposed fix changes which jobs run, including trigger filters, dependency removal, and matrix expansion. Obtain review of these concrete proposed changes before applying them. Cache isolation may reduce warm reuse; prefer an existing unique persistent per-runner directory when available. Budget shard counts against host CPU capacity so parallelism does not increase contention.
