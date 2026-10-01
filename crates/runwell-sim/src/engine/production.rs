use super::*;
use runwell_scheduler::{
    DurationEstimate, FairState, HistoryKey, JobIdentity, NodeStatus, Placement, Production,
    ProductionConfig, ProductionSnapshot,
};
use std::collections::BTreeSet;

impl Engine<'_> {
    pub(super) fn choose(
        &self,
        jobs: &[PendingJob],
        nodes: &[NodeHeadroom],
        now: f64,
    ) -> Result<Option<(Placement, Option<FairState>)>, Error> {
        if self.policy != Policy::Production {
            return Ok(self.selector.select(jobs, nodes, now).map(|p| (p, None)));
        }
        let config = ProductionConfig {
            aging_seconds: self.config.aging_seconds,
            require_runner: self.use_runners,
            ..Default::default()
        };
        let mut snapshot = ProductionSnapshot::default();
        for j in jobs {
            let source = &self.trace.jobs[j.request_id];
            let key = self.history_key(j.request_id);
            snapshot.jobs.insert(
                j.request_id,
                JobIdentity {
                    key: key.clone(),
                    pull_request: source.pull_request.clone(),
                    run: source.run.to_string(),
                    criticality: Some(self.criticality[j.request_id]),
                },
            );
            let mut samples = self
                .duration_windows
                .get(&key)
                .map(|w| w.iter().copied().collect::<Vec<_>>())
                .unwrap_or_default();
            samples.sort_by(f64::total_cmp);
            if !samples.is_empty() {
                snapshot.history.jobs.insert(
                    key,
                    DurationEstimate {
                        p50_seconds: samples[(samples.len() * 50).div_ceil(100) - 1],
                        p90_seconds: samples[(samples.len() * 90).div_ceil(100) - 1],
                        samples: samples.len() as u32,
                        criticality: self.criticality[j.request_id],
                    },
                );
            }
            // The existing replay calibrates work from low-contention successes.
            // Until a replay completion is learned, use that same calibrated input.
        }
        let classes: BTreeSet<_> = jobs
            .iter()
            .map(|j| self.history_key(j.request_id).class)
            .collect();
        let admission = self.config.admission()?;
        let scaled: Vec<_> = nodes
            .iter()
            .cloned()
            .map(|mut n| {
                n.capacity = admission.limit(n.capacity);
                n
            })
            .collect();
        for node in &scaled {
            let active_runs = self
                .running
                .iter()
                .filter(|&&i| self.timings[i].host == Some(node.node_id))
                .map(|&i| {
                    (
                        self.trace.repos[self.trace.jobs[i].repo].clone(),
                        self.trace.jobs[i].run.to_string(),
                    )
                })
                .collect();
            snapshot.nodes.insert(
                node.node_id,
                NodeStatus {
                    admission_open: true,
                    remaining_jobs: u32::MAX,
                    classes: classes.clone(),
                    active_runs,
                },
            );
        }
        let policy = Production::new(&config, &snapshot, &self.fair_state)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        Ok(policy
            .decide(jobs, &scaled, now)
            .map(|d| (d.placement, Some(d.fairness))))
    }
    fn history_key(&self, i: usize) -> HistoryKey {
        let job = &self.trace.jobs[i];
        HistoryKey {
            repo: self.trace.repos[job.repo].clone(),
            workflow_job: job.workflow_job.clone(),
            class: self.trace.classes[job.class].class.clone(),
        }
    }
    pub(super) fn learn_completion(&mut self, i: usize) {
        let t = &self.timings[i];
        if self.policy != Policy::Production
            || t.failure
            || t.cancelled
            || t.host.is_none()
            || self.trace.jobs[i].work <= 0.0
        {
            return;
        }
        let duration = t.end - t.start;
        if duration <= 0.0 {
            return;
        }
        let samples = self
            .duration_windows
            .entry(self.history_key(i))
            .or_default();
        samples.push_back(duration);
        if samples.len() > 128 {
            samples.pop_front();
        }
    }
}
