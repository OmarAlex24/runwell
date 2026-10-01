//! Production snapshots and admission-committed fairness accounting.
use crate::Fleet;
use runwell_admission::Resources;
use runwell_node::Error;
use runwell_scheduler::{
    DurationEstimate, HistoryKey, JobIdentity, NodeHeadroom, NodeStatus, PendingJob, Production,
    ProductionSnapshot, SchedulingPolicy,
};
use runwell_store::{Job, Placement, SchedulingContext};
use runwell_transport::{Request, Response};
use std::collections::BTreeSet;

pub(crate) async fn context(
    store: &runwell_store::Store,
    job: &Job,
) -> Result<SchedulingContext, Error> {
    Ok(store
        .scheduling_context(job.id)
        .await?
        .unwrap_or(SchedulingContext {
            workflow_job: format!("display:{}", job.metadata.name),
            ready_at_ms: store.queued_at(job.id).await?,
            ..Default::default()
        }))
}
pub(crate) fn history_key(job: &Job, context: &SchedulingContext) -> HistoryKey {
    HistoryKey {
        repo: job.metadata.repo.to_lowercase(),
        workflow_job: context.workflow_job.clone(),
        class: job.metadata.class.clone(),
    }
}
pub(crate) fn identity(job: &Job, context: &SchedulingContext) -> JobIdentity {
    JobIdentity {
        key: history_key(job, context),
        pull_request: if context.pull_request.is_empty() {
            format!("run:{}", job.metadata.workflow_run_id)
        } else {
            context.pull_request.clone()
        },
        run: job.metadata.workflow_run_id.to_string(),
        criticality: context.criticality,
    }
}
impl Fleet {
    /// Enable the production SchedulingPolicy; custom policy injection remains available for tests.
    pub fn with_production_policy(mut self, config: runwell_scheduler::ProductionConfig) -> Self {
        self.production = Some(config);
        self
    }
    pub(crate) async fn history(&self) -> Result<runwell_scheduler::HistorySnapshot, Error> {
        let seconds = self
            .config
            .network
            .as_ref()
            .ok_or(Error::Config)?
            .expected_seconds as f64;
        Ok(self
            .store
            .duration_history(
                self.config
                    .controller
                    .classes
                    .iter()
                    .map(|c| {
                        (
                            c.name.clone(),
                            DurationEstimate {
                                p50_seconds: seconds,
                                p90_seconds: seconds,
                                samples: 0,
                                criticality: Default::default(),
                            },
                        )
                    })
                    .collect(),
            )
            .await?)
    }
    pub(crate) async fn production_select(&self, jobs: &[Job]) -> Result<Vec<i64>, Error> {
        let config = self.production.as_ref().ok_or(Error::Config)?;
        let mut chosen = Vec::new();
        // A reply can be lost after admission. Resolve that intent before another
        // proposal, so later accepted accounting can never overwrite it.
        for id in self.store.pending_dispatches().await? {
            let job = self.store.job(id).await?;
            if job.state.terminal() {
                // Resilience owns cleanup of fenced/cancelled reservations.
                self.store.abandon_dispatch(id).await?;
                continue;
            }
            if self.confirm_dispatch(&job).await? {
                chosen.push(id);
            }
        }
        let existing = self.store.placements().await?;
        for job in jobs {
            if !chosen.contains(&job.id) && existing.iter().any(|p| p.job_id == job.id && !p.lost) {
                chosen.push(job.id);
            }
        }
        let reports = self.available().await?;
        let mut snapshot = ProductionSnapshot {
            history: self.history().await?,
            ..Default::default()
        };
        let mut nodes = Vec::new();
        for (index, (id, report)) in reports.iter().enumerate() {
            let mut active_runs = BTreeSet::new();
            let mut reserved = Resources {
                cpu_slots: report.reserved_cpu,
                memory_bytes: report.reserved_memory,
            };
            let mut slots = report.free_slots;
            for placement in existing.iter().filter(|p| &p.node_id == id) {
                let job = self.store.job(placement.job_id).await?;
                let reported = report.jobs.iter().any(|s| s.key.job_id == job.id);
                if job.state.terminal() && !reported {
                    continue;
                }
                active_runs.insert((
                    job.metadata.repo.to_lowercase(),
                    job.metadata.workflow_run_id.to_string(),
                ));
                if !reported {
                    reserved.cpu_slots =
                        reserved.cpu_slots.saturating_add(job.metadata.reserved_cpu);
                    reserved.memory_bytes = reserved
                        .memory_bytes
                        .saturating_add(job.metadata.reserved_memory);
                    slots = slots.saturating_sub(1);
                }
            }
            snapshot.nodes.insert(
                index,
                NodeStatus {
                    admission_open: !report.draining,
                    remaining_jobs: slots,
                    classes: self
                        .config
                        .controller
                        .classes
                        .iter()
                        .map(|c| c.name.clone())
                        .collect(),
                    active_runs,
                },
            );
            nodes.push(NodeHeadroom {
                node_id: index,
                class: String::new(),
                capacity: Resources {
                    cpu_slots: report.cpu_slots,
                    memory_bytes: report.memory_bytes,
                },
                reserved,
                free_runners: vec![slots as usize],
            });
        }
        let mut pending = Vec::new();
        for job in jobs
            .iter()
            .filter(|j| !existing.iter().any(|p| p.job_id == j.id))
        {
            let context = context(&self.store, job).await?;
            let meta = identity(job, &context);
            let estimate = snapshot.history.estimate(&meta.key).ok_or(Error::Config)?;
            snapshot.jobs.insert(job.id as usize, meta);
            pending.push(PendingJob {
                request_id: job.id as usize,
                pool: 0,
                host_class: None,
                ready_at: context.ready_at_ms as f64 / 1000.0,
                expected_seconds: estimate.p50_seconds,
                critical_path_seconds: estimate.p90_seconds,
                fair_service: 0.0,
                reservation: Resources {
                    cpu_slots: job.metadata.reserved_cpu,
                    memory_bytes: job.metadata.reserved_memory,
                },
            });
        }
        let mut fairness = self.store.fairness().await?;
        let now = self.clock.now_ms();
        loop {
            let policy =
                Production::new(config, &snapshot, &fairness).map_err(|_| Error::Config)?;
            let Some(selected) =
                SchedulingPolicy::select(&policy, &pending, &nodes, now as f64 / 1000.0)
            else {
                break;
            };
            let decision = policy
                .decide(&pending, &nodes, now as f64 / 1000.0)
                .ok_or(Error::Config)?;
            let job = jobs
                .iter()
                .find(|j| j.id as usize == selected.request_id)
                .ok_or(Error::Config)?;
            let node = nodes
                .iter_mut()
                .find(|n| n.node_id == selected.node_id)
                .ok_or(Error::Config)?;
            self.store
                .propose_dispatch(
                    Placement {
                        job_id: job.id,
                        attempt: 1,
                        node_id: reports[selected.node_id].0.clone(),
                        assigned_at: now,
                        lost: false,
                        execution_started_at: None,
                    },
                    decision.fairness.clone(),
                )
                .await?;
            if self.confirm_dispatch(job).await? {
                fairness = decision.fairness;
                chosen.push(job.id);
                node.reserved.cpu_slots = node
                    .reserved
                    .cpu_slots
                    .saturating_add(job.metadata.reserved_cpu);
                node.reserved.memory_bytes = node
                    .reserved
                    .memory_bytes
                    .saturating_add(job.metadata.reserved_memory);
                node.free_runners[0] = node.free_runners[0].saturating_sub(1);
                let status = snapshot.nodes.get_mut(&node.node_id).ok_or(Error::Config)?;
                status.remaining_jobs = status.remaining_jobs.saturating_sub(1);
                status.active_runs.insert((
                    job.metadata.repo.to_lowercase(),
                    job.metadata.workflow_run_id.to_string(),
                ));
            } else {
                // A stale report must not repeatedly select this rejecting host.
                snapshot
                    .nodes
                    .get_mut(&node.node_id)
                    .ok_or(Error::Config)?
                    .admission_open = false;
            }
            pending.retain(|j| j.request_id != selected.request_id);
        }
        Ok(chosen)
    }
    async fn confirm_dispatch(&self, job: &Job) -> Result<bool, Error> {
        match self
            .rpc(job.id as u64, |key| Request::Admit {
                key,
                job: job.clone(),
            })
            .await?
        {
            Response::Admitted(true) => {
                if self.store.accept_dispatch(job.id).await? {
                    self.hooks.admission(&job.metadata.class, true);
                }
                Ok(true)
            }
            Response::Admitted(false) => {
                self.store.reject_dispatch(job.id).await?;
                self.hooks.admission(&job.metadata.class, false);
                Ok(false)
            }
            _ => Err(Error::Config),
        }
    }
    pub(crate) async fn watchdog_seconds(&self, job: &Job) -> Result<f64, Error> {
        let fallback = self
            .config
            .network
            .as_ref()
            .ok_or(Error::Config)?
            .expected_seconds as f64;
        let key = history_key(job, &context(&self.store, job).await?);
        Ok(self
            .store
            .duration_estimate(&key)
            .await?
            .map_or(fallback, |e| e.p90_seconds))
    }
}
