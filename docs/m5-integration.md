# Production controller integration

The network controller runs the pure `Production` scheduling policy over a fresh
materialized duration snapshot per scheduling batch. Successful completions update
the store's windowed p50/p90 estimates. The execution watchdog uses learned p90,
with `network.expected_seconds` as the cold-start class default.

Placement intent and proposed fairness accounting are committed before admission.
Only a confirmed node admission advances durable fairness. A lost reply is
reconciled on the same node before proposing another placement. A definitive
rejection releases the proposal without charging fairness. A fenced or cancelled
intent retains placement ownership for cleanup, then discards pending accounting.
The policy uses admission-reported capacity without adding another overcommit
multiplier. It fills both available nodes while accounting for unreported leases.

`Store::set_scheduling_context` accepts exact workflow keys, PR buckets and DAG
criticality from an enricher. Queue events preserve their ready timestamps and
use a namespaced workflow-reference/display-name key, since the scale-set feed
has no YAML job key or PR/DAG metadata. Without enrichment, fairness uses a bucket
per run and criticality falls back to learned history. Statistical demand lacking
repository identity shares a fallback bucket until actual execution is bound.
GitHub still chooses which queued request a matching idle runner executes.

## Retry and history

The failure, lost-node, watchdog and completion hooks journal observations before
acknowledging delivery. A separate controller task resolves the actual unique
runner name to a numeric REST job ID and run attempt through the
[attempt job inventory](https://docs.github.com/en/rest/actions/workflow-jobs#list-jobs-for-a-workflow-run-attempt).
The scale-set job UUID and local placement attempt are never treated as REST IDs.
Missing, stale or ambiguous identity remains deferred. No display-name matching
can authorize a retry.

OOM, node loss and independently missing runner heartbeats supply positive
classifier evidence. Generic backend errors, process exit codes and duration
watchdogs remain unknown unless more specific evidence is available. Failed
workflow steps conservatively veto retries; the live REST adapter does not fetch
full logs. This can miss valid infrastructure retries but prevents a failed test
from being retried merely because the node also failed. The execution-source port
can supply bounded diagnostic evidence for richer integrations.

The `runwell-retry` policy checks every failed job in the completed run attempt,
claims durably, and issues at most one logical rerun. Mixed test/infra runs,
unknown failures, stale attempts, exhausted daily caps and automatic retry chains
remain blocked. Retry defaults off. Claims survive controller restart and
ambiguous HTTP outcomes. Completion history is idempotent by REST job/attempt.
Hook redelivery does not inflate completion counters; a crash between the durable
counter claim and the in-memory increment may lose an increment.

## Monitoring and operation

`[controller.production]` configures `metrics_listen` (default loopback port 9090),
`retry_enabled`, `retry_daily_cap`, `aging_seconds`, `repository_weights`,
`tick_seconds`, `github_api_base`, and an optional `alert_webhook`.
The independent HTTP listener exposes `/metrics` in OpenMetrics format. Its
labels are restricted to configured classes/nodes. Keep non-loopback listeners
behind the deployment's network controls.

Alert evaluation reads durable completion observations, learned p90 job watches,
controller receipt times for node reports, and release expiry dates derived from
the oldest newer stable runner release. It runs even without a webhook, logging
new active alerts. Webhook delivery uses the M5a deduplication, cooldown and
bounded backoff implementation outside the admission loop. Webhook cooldown state
is process-local; receivers should honor the idempotency key.

`tests/production.rs` drives two real in-process node agents over fake RPC links,
a burst from two repositories, learned criticality and duration ordering, fair
share, OOM versus test results, one durable retry, duplicate delivery, restart,
resource cleanup, and a real HTTP metrics scrape. A second test loses an admission
reply and verifies fairness is charged once after recovery. REST contract tests
cover unique runner binding, actual run attempts and failed-step vetoes.

A real two-host run must still validate certificate distribution/rotation,
systemd/cgroup limits, Docker/overlay cleanup, runner renewal, live GitHub
assignment and rerun permissions, network partitions, drain/restart behavior,
monitoring delivery, and representative burst latency. Privileged Linux tests
compile in ordinary container validation but need the documented host facilities
to execute.
