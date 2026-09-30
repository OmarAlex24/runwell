//! Terminal-friendly Markdown renderer. Tables escape all untrusted trace text.
use crate::model::Report;

fn cell(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
        .replace('`', "\\`")
}
fn minutes(value: Option<f64>) -> String {
    value
        .map(|v| format!("{:.2}", v / 60.0))
        .unwrap_or_else(|| "—".into())
}

pub fn markdown(report: &Report) -> String {
    let f = &report.failures;
    let run_count: usize = report.runs.iter().map(|r| r.end_to_end_seconds.count).sum();
    let mut out = format!(
        "# CI time report\n\n{} successful first-attempt runs; {} jobs in the window. Clear infra: **{:.2}%**; confirmed flaky: **{:.2}%** ({} executed non-aggregator jobs). Combined: **{:.2}%**; target <1%: **{}**.\n\nWindow: {} to {} (exclusive). Markdown durations are minutes.\n",
        run_count,
        report.window.jobs,
        f.infra_percent,
        f.flaky_percent,
        f.eligible_jobs,
        f.infra_and_flaky_percent,
        if f.eligible_jobs == 0 {
            "no data"
        } else if f.under_one_percent {
            "below target on observed evidence"
        } else {
            "above target"
        },
        report.window.since,
        report.window.until
    );
    out.push_str("\n## End-to-end and achievable floor\n\nSuccessful first attempts, run created → last executed job completed. Best case removes queue and in-job waits and uses low-concurrency execution factors. Half-baseline columns are the 2× improvement targets for this input window.\n\n| Repo | Event | Runs | E2E p50 | p90 | p99 | Floor p50 | p90 | Half p50 | Half p90 | Paths needs / inferred |\n|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for r in &report.runs {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} / {} |\n",
            cell(&r.repo),
            cell(&r.event),
            r.end_to_end_seconds.count,
            minutes(r.end_to_end_seconds.p50),
            minutes(r.end_to_end_seconds.p90),
            minutes(r.end_to_end_seconds.p99),
            minutes(r.best_case_seconds.p50),
            minutes(r.best_case_seconds.p90),
            minutes(r.half_baseline_p50_seconds),
            minutes(r.half_baseline_p90_seconds),
            r.needs_runs,
            r.inferred_runs
        ));
    }
    out.push_str("\n## Critical-path composition\n\nMeans within E2E percentile bands; work excludes matching in-job wait steps.\n\n| Repo / event | Band | Runs | E2E | Queue | Work | Dispatch gap | In-job wait |\n|---|---|---:|---:|---:|---:|---:|---:|\n");
    for r in &report.runs {
        for b in &r.bands {
            out.push_str(&format!(
                "| {} / {} | {} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} |\n",
                cell(&r.repo),
                cell(&r.event),
                cell(&b.band),
                b.count,
                b.end_to_end_seconds / 60.0,
                b.queue_seconds / 60.0,
                b.work_seconds / 60.0,
                b.dispatch_gap_seconds / 60.0,
                b.in_job_wait_seconds / 60.0
            ));
        }
    }
    out.push_str("\n## Per-job timing\n\nFail/cancel rates exclude skipped jobs; duration and runner-minutes use jobs that ran.\n\n| Repo / workflow | Job | Count / ran | Duration p50 / p90 | Queue p50 / p90 | Fail % | Cancel % | Runner-min |\n|---|---|---:|---:|---:|---:|---:|---:|\n");
    for j in &report.jobs {
        out.push_str(&format!(
            "| {} / {} | {} | {} / {} | {} / {} | {} / {} | {:.2} | {:.2} | {:.1} |\n",
            cell(&j.repo),
            cell(&j.workflow),
            cell(&j.job),
            j.count,
            j.ran,
            minutes(j.duration_seconds.p50),
            minutes(j.duration_seconds.p90),
            minutes(j.queue_seconds.p50),
            minutes(j.queue_seconds.p90),
            j.fail_percent,
            j.cancel_percent,
            j.runner_minutes
        ));
    }
    let c = &report.concurrency;
    out.push_str(&format!("\n## Host concurrency\n\nPeak: **{}**; idle: **{:.1}%**; busy: **{:.2} hours**; clipped same-runner overlaps: {}. Scope: {}.\n\n| Concurrent jobs | Hours | Wall % | Busy % |\n|---:|---:|---:|---:|\n",
        c.peak,c.idle_fraction*100.0,c.busy_seconds/3600.0,c.clipped_runner_overlaps,
        if report.configuration.host_labels.is_empty() {"all observed runners".into()} else {cell(&report.configuration.host_labels.join(", "))}));
    for l in &c.distribution {
        out.push_str(&format!(
            "| {} | {:.2} | {:.1} | {:.1} |\n",
            l.level,
            l.seconds / 3600.0,
            l.wall_fraction * 100.0,
            l.busy_fraction * 100.0
        ));
    }
    out.push_str("\n| Runner label | Capacity | Source | Peak | Saturation hours |\n|---|---:|---|---:|---:|\n");
    for l in &c.labels {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {:.2} |\n",
            cell(&l.label),
            l.capacity,
            if l.capacity_inferred {
                "observed runner count"
            } else {
                "configured"
            },
            l.peak,
            l.saturation_hours
        ));
    }
    out.push_str(&format!("\n## Contention\n\nSuccessful job durations exclude matching in-job waits. Samples are grouped by each job's time-weighted host concurrency: low 1–{}, high ≥{}. Steps use their parent job's concurrency band.\n\n| Repo | Job / step | Low n | Low p50 | High n | High p50 | High / low |\n|---|---|---:|---:|---:|---:|---:|\n",report.configuration.low_concurrency,report.configuration.high_concurrency));
    for c in &report.contention {
        let name = match &c.step {
            Some(s) => format!("{} / {}", c.job, s),
            None => c.job.clone(),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            cell(&c.repo),
            cell(&name),
            c.low.count,
            minutes(c.low.p50),
            c.high.count,
            minutes(c.high.p50),
            c.high_to_low_ratio
                .map(|v| format!("{v:.2}×"))
                .unwrap_or_else(|| "—".into())
        ));
    }
    out.push_str(&format!("\n## Failure classification\n\n| Class | Jobs |\n|---|---:|\n| Clear infra | {} |\n| Confirmed flaky | {} |\n| Code | {} |\n| Superseded | {} |\n| Unknown | {} |\n| Cancelled before start (excluded) | {} |\n| Aggregators (excluded, all conclusions) | {} |\n\nUnknown cancellations and missing evidence require review; the observed rate alone does not certify the target. Flaky rerun evidence does not by itself establish an infrastructure cause.\n",f.infra,f.flaky,f.code,f.superseded,f.unknown,f.cancelled_before_start,f.aggregators));
    if !report.warnings.is_empty() {
        out.push_str("\n## Method and limitations\n\n");
        for warning in &report.warnings {
            out.push_str(&format!("- {}\n", cell(warning)));
        }
    }
    out
}
