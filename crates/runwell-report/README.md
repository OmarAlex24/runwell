# runwell report

Explain where CI time goes and measure infrastructure reliability across any repositories.

```sh
runwell report --repo acme/app --repo acme/api --since 7d --event pull_request
runwell report --repo acme/app --since 2026-01-01 --until 2026-01-08 --export-trace history.jsonl
runwell report --from-trace history.jsonl --format json
runwell report --from-trace history.jsonl --host-label host-a --label-capacity pool-a=6
```

Authentication uses `GH_TOKEN`, `GITHUB_TOKEN`, then `gh auth token`. Offline mode requires none of them and makes no requests. Live input defaults to the preceding seven days. Offline input defaults to the complete observed trace window. Explicit dates/timestamps work in both modes; `--until` is exclusive. Relative `--since` durations are measured backwards from `--until`.

API run searches are split when they exceed GitHub's 1,000-result search cap. All run attempts, jobs, and check-run annotations are paginated. Raw responses are cached in private files under `--cache-dir` (default `$XDG_CACHE_HOME/runwell/report` or `$HOME/.cache/runwell/report`). Mutable responses expire after five minutes; workflow YAML keyed by commit SHA is immutable. Rate-limit responses and exhausted rate-limit headers delay subsequent requests. Permission errors are reported without repeated rate-limit retries.

`--export-trace` writes all collected jobs before report filters so the result remains reusable. New trace fields are serde-defaulted. The portable JSONL remains compatible with older consumers.

## Metrics and interpretation

- E2E: run creation to last executed job completion, for successful runs that were never rerun. Groups are repository and event; all workflows in those groups count. Per-job tables retain workflow identity.
- Critical path: dependency names from workflow YAML at the commit SHA when the mapping is complete and unambiguous. Matrix jobs, reusable workflows, dynamic names, unavailable YAML, or invalid graphs fall back to timestamps. Inference walks backwards using the latest predecessor completion at or before job creation plus three seconds, constrained to finish before the current job starts.
- Composition: mean queue, work, dispatch gap, and in-job wait within ≤p50, p40–60, p85–95, and ≥p90 E2E bands. Work includes job time outside steps. Matching wait intervals are unioned and clipped to job boundaries. Repeat `--wait-regex` to replace `(?i)(wait|slot|semaphore|lock)`.
- Concurrency: job start/completion sweep, with overlaps on the same runner clipped. Idle time uses the requested observation window. Labels can overlap; their saturation times must not be summed. Configure capacity with repeated `--label-capacity LABEL=N`; otherwise capacity is the distinct observed runner count, explicitly labeled as inferred.
- Host scope: `--host-label` limits concurrency and contention to jobs carrying every supplied label. `--label` scopes all reported metrics, and `--event`/`--workflow` scope reported metrics; the host sweep still includes every event and job label on that host within the selected repositories. Selecting one repository cannot reveal contention from repositories absent from the input.
- Contention: successful job execution minus wait steps, and individual successful step durations, grouped by the parent job's time-weighted host concurrency. Defaults: low 1–3, high ≥7; both thresholds are configurable.
- Best case: re-simulate the full observed DAG with zero queue, zero matching in-job waits, measured dispatch gaps, and per-job execution scaled by `min(1, low-concurrency p50 / overall p50)`. Jobs without low-concurrency samples retain measured execution. This is an empirical estimate; it does not model YAML matrix parallelism or hypothetical workflow changes. Each run's graph is retained.
- Reliability: denominator is executed non-aggregator job attempts. Jobs without runner assignment/start and skipped jobs are excluded. Executed attempts with inconsistent creation/start timestamps still count in reliability rates; invalid timing samples are excluded from execution/queue metrics. Aggregators have only set-up and complete steps, or match repeated `--aggregator-regex` patterns. Clear infra and confirmed flaky are reported separately and combined against the <1% target. Flaky means a later attempt or run of the same SHA and job succeeded; missing SHAs restrict detection to the same run ID. This evidence alone does not prove infrastructure caused the flake. Unknown cancellations remain visible.

Repeat `--repo` to select multiple repositories and `--workflow` to select CI workflow display names. Repeat `--label` with `--host-label` when every metric should describe the selected host. For precise historical classification, export annotations and request failed-job logs with `--fetch-logs N`; the trace retains this evidence for offline analysis.

## Classification rules

The shipped [default-rules.toml](default-rules.toml) is the reference rule set. `--rules custom.toml` replaces it; regexes are ordered and operate on each failed-step name prefixed with `failed step: `, annotation message, and optional log text. Infrastructure evidence precedes inferred flakiness. Additional aggregator expressions can also be supplied in the rules file.

```toml
aggregator_patterns = ['^summary$']

[[rules]]
name = "runner disconnected"
class = "infra"
pattern = '(?i)lost communication with the server'
```

Classes: `infra`, `flaky`, `code`, `superseded`, `unknown`. Regexes and classes are validated before API collection. Rate claims from traces missing evidence are explicitly lower bounds.

## Output contract

JSON uses `schemaVersion: 1`, camelCase field names, seconds for duration/queue/percentile fields, minutes only for `runnerMinutes`, hours for `saturationHours`, percent units for `*Percent`, and 0–1 fractions for concurrency distribution/idle fields. Missing percentiles are `null`, never zero. `runs[].halfBaselineP50Seconds` and `halfBaselineP90Seconds` give the half-latency targets for the current input window; comparing separate baseline and post-change windows remains necessary to establish improvement.

Markdown is a short summary followed by timing, composition, concurrency, contention, and failure tables. Names are escaped for table safety. No synthetic test fixture contains production history.
