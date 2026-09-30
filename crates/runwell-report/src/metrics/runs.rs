use super::{
    bestcase::{self, Factors},
    critical, duration, percentile, percentiles, seconds,
};
use crate::model::{Band, RunSummary};
use regex::RegexSet;
use runwell_trace::TraceJob;
use std::collections::{BTreeMap, BTreeSet};

pub fn summarize(jobs: &[TraceJob], waits: &RegexSet, factors: &Factors) -> Vec<RunSummary> {
    // The reference population is runs that remained on their first attempt.
    // This also avoids attributing a latest-attempt success to old trace records
    // whose run_conclusion was copied from the final run metadata.
    let rerun_ids: BTreeSet<_> = jobs
        .iter()
        .filter(|j| j.run_attempt > 1)
        .map(|j| (&j.repo, j.run_id))
        .collect();
    let mut attempts = BTreeMap::<_, Vec<_>>::new();
    for j in jobs
        .iter()
        .filter(|j| j.run_attempt == 1 && !rerun_ids.contains(&(&j.repo, j.run_id)))
    {
        attempts.entry((&j.repo, j.run_id)).or_default().push(j);
    }
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    for ((repo, _), all) in attempts {
        if !all
            .iter()
            .all(|j| j.run_conclusion.as_deref() == Some("success"))
        {
            continue;
        }
        let Some(created) = all.iter().filter_map(|j| j.run_created_at).min() else {
            continue;
        };
        let active: Vec<_> = all
            .iter()
            .copied()
            .filter(|j| duration(j).is_some())
            .collect();
        let Some(end) = active.iter().filter_map(|j| j.completed_at).max() else {
            continue;
        };
        let graph = critical::graph(&active);
        let comp = critical::composition(&active, &graph, waits);
        let floor = bestcase::estimate(&active, &graph, factors, waits);
        let event = all[0].event.as_deref().unwrap_or("unknown");
        groups.entry((repo, event)).or_default().push((
            seconds(created, end),
            comp,
            floor,
            graph.explicit,
        ));
    }
    groups
        .into_iter()
        .map(|((repo, event), rows)| {
            let e: Vec<_> = rows.iter().map(|r| r.0).collect();
            let floors: Vec<_> = rows.iter().map(|r| r.2).collect();
            let mut bands = Vec::new();
            for (name, lo, hi) in [
                ("<=p50", 0.0, 0.5),
                ("p40-p60", 0.4, 0.6),
                ("p85-p95", 0.85, 0.95),
                (">=p90", 0.9, 1.0),
            ] {
                let lower = percentile(&e, lo).unwrap_or(0.0);
                let upper = percentile(&e, hi).unwrap_or(0.0);
                let selected: Vec<_> = rows
                    .iter()
                    .filter(|r| r.0 >= lower && r.0 <= upper)
                    .collect();
                if selected.is_empty() {
                    continue;
                }
                let n = selected.len() as f64;
                bands.push(Band {
                    band: name.into(),
                    count: selected.len(),
                    end_to_end_seconds: selected.iter().map(|r| r.0).sum::<f64>() / n,
                    queue_seconds: selected.iter().map(|r| r.1.queue).sum::<f64>() / n,
                    work_seconds: selected.iter().map(|r| r.1.work).sum::<f64>() / n,
                    dispatch_gap_seconds: selected.iter().map(|r| r.1.gap).sum::<f64>() / n,
                    in_job_wait_seconds: selected.iter().map(|r| r.1.wait).sum::<f64>() / n,
                });
            }
            let observed = percentiles(&e);
            RunSummary {
                repo: repo.clone(),
                event: event.into(),
                best_case_seconds: percentiles(&floors),
                half_baseline_p50_seconds: observed.p50.map(|v| v / 2.0),
                half_baseline_p90_seconds: observed.p90.map(|v| v / 2.0),
                end_to_end_seconds: observed,
                needs_runs: rows.iter().filter(|r| r.3).count(),
                inferred_runs: rows.iter().filter(|r| !r.3).count(),
                bands,
            }
        })
        .collect()
}
