use super::*;
use std::fmt::Write;
impl Report {
    /// Render calibration first, then every policy/scenario and classic allocation.
    pub fn markdown(&self) -> String {
        let mut out = String::from(
            "Times are minutes. Queue is critical-path waiting share; failure counts are all-cause-derived proxies.\n\n",
        );
        out.push_str("Calibration against observed matching cohorts (one host, baseline):\n\n| Repo | Event | Observed p50 | Observed p90 | Observed queue | p50 error | p90 error | Queue error pp | Calibrated |\n|---|---|---:|---:|---:|---:|---:|---:|---|\n");
        for c in &self.calibration {
            let _ = writeln!(
                out,
                "| {} | {} | {:.2} | {:.2} | {:.1}% | {:+.1}% | {:+.1}% | {:+.1} | {} |",
                escape(&c.repo),
                escape(&c.event),
                c.observed.p50_minutes,
                c.observed.p90_minutes,
                c.observed.queue_share * 100.0,
                c.p50_error * 100.0,
                c.p90_error * 100.0,
                c.queue_error_pp,
                if c.within_ten_percent { "yes" } else { "MISS" }
            );
        }
        if self
            .calibration
            .iter()
            .any(|c| c.event == "pull_request" && !c.within_ten_percent)
        {
            out.push_str("\nCalibration failed. Scenario rankings are exploratory. Cancelled runs are censored, never counted as fast successful completions.\n");
        }
        out.push_str("\n| Hosts | Policy | Overcommit | Repo | Event | Completed | Cancelled | p50 | p90 | p99 | Queue | CPU | RAM | Failures |\n|---:|---|---:|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
        for r in &self.rows {
            let m = &r.metrics;
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {:.2} | {:.2} | {:.2} | {:.1}% | {:.1}% | {:.1}% | {} |",
                r.hosts,
                r.policy.name(),
                r.overcommit
                    .map_or_else(|| "—".into(), |v| format!("{v:.2}")),
                escape(&r.repo),
                escape(&r.event),
                m.runs,
                m.cancelled_runs,
                m.p50_minutes,
                m.p90_minutes,
                m.p99_minutes,
                m.queue_share * 100.0,
                r.mean_cpu_utilization * 100.0,
                r.mean_memory_utilization * 100.0,
                r.infra_failures
            );
        }
        for e in &self.equivalence {
            let _ = writeln!(
                out,
                "\nEquivalent sanity check, {} hosts: {}; max job timing difference {:.9} s.",
                e.hosts,
                if e.passed { "PASS" } else { "FAIL" },
                e.max_timing_error_seconds
            );
        }
        if let Some(search) = &self.classic {
            let _ = writeln!(
                out,
                "\nClassic search: {} evaluated, {} rejected (infeasible or censored), bounds {:?}, p90 targets {:?}. Top {} per semaphore mode.\n",
                search.evaluated,
                search.rejected,
                search.max_runners_per_host,
                search.target_p90_minutes,
                search.top_per_mode
            );
            out.push_str("| Rank | Semaphore | Host × repo runners | Score | Repo | p50 | p90 | p99 | Queue |\n|---:|---|---|---:|---|---:|---:|---:|---:|\n");
            let mut ranks = BTreeMap::new();
            for a in &search.allocations {
                let rank = ranks.entry(a.heavy_slots).or_insert(0);
                *rank += 1;
                for r in a.rows.iter().filter(|r| r.event == "pull_request") {
                    let _ = writeln!(
                        out,
                        "| {} | {} | {:?} | {:.3} | {} | {:.2} | {:.2} | {:.2} | {:.1}% |",
                        rank,
                        a.heavy_slots
                            .map_or_else(|| "off".into(), |s| s.to_string()),
                        a.runners,
                        a.score,
                        escape(&r.repo),
                        r.metrics.p50_minutes,
                        r.metrics.p90_minutes,
                        r.metrics.p99_minutes,
                        r.metrics.queue_share * 100.0
                    );
                }
            }
        }
        let d = &self.diagnostics;
        let _ = writeln!(
            out,
            "\n{} input records; {} excluded; {} local executions; {} external jobs; {} workflow-resolved jobs; {} inferred jobs; {} unsupported intrinsic estimates; {} superseded runs.\n",
            d.input_jobs,
            d.excluded_jobs,
            d.local_jobs,
            d.external_jobs,
            d.workflow_jobs,
            d.inferred_jobs,
            d.unsupported_intrinsic_jobs,
            d.superseded_runs
        );
        for warning in &d.warnings {
            let _ = writeln!(out, "- {warning}");
        }
        out
    }
}
fn escape(value: &str) -> String {
    value.replace('|', "\\|").replace(['\r', '\n'], " ")
}
