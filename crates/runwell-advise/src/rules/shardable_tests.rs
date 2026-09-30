//! Long whole-suite tests dominate latency and amplify CPU contention. Detect
//! recognized tools/wrappers; trace mode requires critical membership and >3 min
//! job median, with step dominance when step timings exist. Guard existing shards
//! and deploy gates. Suggest two self-hosted shards, four hosted, without dropping tests.
use super::*;
use crate::{
    context::test_tool,
    model::Savings,
    timing::{Trace, percentile},
};
pub(super) fn check(job: &Job<'_>, trace: Option<&Trace>, out: &mut Vec<Finding>) {
    if job.gate() {
        return;
    }
    let timing = trace.and_then(|t| t.timing(job));
    if trace.is_some()
        && !timing
            .as_ref()
            .is_some_and(|t| t.work_p50 > 3.0 && t.critical_samples > 0)
    {
        return;
    }
    for (i, step) in job.steps().iter().enumerate() {
        let Some(tool) = test_tool(step, job) else {
            continue;
        };
        let values = trace.map_or(vec![], |t| t.step_minutes(job, step.str("name")));
        let step50 = percentile(&values, 0.5);
        if !values.is_empty() && timing.as_ref().is_some_and(|t| step50 < t.work_p50 * 0.4) {
            continue;
        }
        let count = if job.hosted() { 2 } else { 4 };
        let command = match tool {
            "go" => "# Split every package into disjoint shards; preserve existing race/tags/coverage flags.\n          packages=$(go list ./... | awk -v shard=${{ matrix.shard }} -v count=SHARDS '(NR-1)%count == shard-1')\n          [ -z \"$packages\" ] || go test -p 2 $packages",
            "pytest" => "pytest --splits SHARDS --group ${{ matrix.shard }} # requires pytest-split; preserve options",
            "nextest" => "cargo nextest run --partition hash:${{ matrix.shard }}/SHARDS # verify nextest compatibility",
            "cargo" => "# Split all workspace packages; retain existing feature/profile/coverage flags and doctests.\n          cargo metadata --no-deps --format-version 1 | jq -r '. as $m | $m.packages[] | select(.id as $id | $m.workspace_members | index($id)) | .name' | awk -v shard=${{ matrix.shard }} -v count=SHARDS '(NR-1)%count == shard-1' | while read -r package; do cargo test -p \"$package\" || exit $?; done",
            "jest" => "jest --shard=${{ matrix.shard }}/SHARDS",
            "vitest" => "vitest run --shard=${{ matrix.shard }}/SHARDS",
            "playwright" => "playwright test --shard=${{ matrix.shard }}/SHARDS",
            "rspec" => "# Split all spec files into disjoint groups with parallel_tests; preserve options.\n          parallel_rspec -n SHARDS --only-group ${{ matrix.shard }}",
            "phpunit" => "# Create SHARDS complete, disjoint PHPUnit XML configurations; preserve every test and option.\n          phpunit --configuration phpunit-shard-${{ matrix.shard }}.xml",
            _ => "# Inspect the test script and forward the underlying tool's native shard flag.\n          <existing-test-command> --shard=${{ matrix.shard }}/SHARDS",
        }.replace("SHARDS",&count.to_string());
        let snippet = format!(
            "strategy:\n  fail-fast: false\n  matrix:\n    shard: [{}]\nsteps:\n  - run: |\n          {command}\n# Retain all setup, test flags, service isolation, and aggregate coverage/results.",
            (1..=count)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut f = job_finding(
            job,
            step,
            "shardable-tests",
            "A whole-suite test step can be partitioned to shorten the critical path. Budget shards against available CPU capacity.",
            &snippet,
            json!({"tool":tool,"shards":count,"timing":timing,"stepP50Minutes":step50,"stepP90Minutes":percentile(&values,0.9),"estimateAssumption":"balanced shards, adequate capacity, excludes setup and queue"}),
        );
        f.step = Some(step_name(step, i));
        if !values.is_empty() {
            f.estimated_savings = Some(Savings {
                p50: step50 * (1.0 - 1.0 / count as f64),
                p90: percentile(&values, 0.9) * (1.0 - 1.0 / count as f64),
            });
        }
        out.push(f);
    }
}
