use crate::Fleet;
use runwell_node::{Error, JobPlan, LocalJob, NodeBackend, NodeFuture, ProcessState};
use runwell_runner::LaunchSpec;
use runwell_store::{Job, JobMeasurement};
use runwell_transport::{Request, Response};
use secrecy::ExposeSecret;
use std::collections::{BTreeMap, HashSet};

impl NodeBackend for Fleet {
    fn manages_admission(&self) -> bool {
        true
    }
    fn initialize(&self) -> NodeFuture<'_, ()> {
        Box::pin(self.poll())
    }
    fn maintenance(&self) -> NodeFuture<'_, ()> {
        Box::pin(self.maintain())
    }
    fn schedule<'a>(&'a self, jobs: &'a [Job], _now: u64) -> NodeFuture<'a, Vec<i64>> {
        Box::pin(self.select(jobs))
    }
    fn admit<'a>(&'a self, job: &'a Job) -> NodeFuture<'a, bool> {
        Box::pin(async move {
            match self
                .rpc(job.id as u64, |key| Request::Admit {
                    key,
                    job: job.clone(),
                })
                .await?
            {
                Response::Admitted(yes) => Ok(yes),
                _ => Err(Error::Config),
            }
        })
    }
    fn fleet_capacities<'a>(
        &'a self,
        classes: &'a BTreeMap<i64, runwell_config::JobClass>,
    ) -> NodeFuture<'a, Option<BTreeMap<i64, u32>>> {
        Box::pin(async move { Ok(Some(self.capacities(classes).await?)) })
    }
    fn pressure(&self) -> NodeFuture<'_, runwell_admission::Pressure> {
        Box::pin(async { Ok(Default::default()) })
    }
    fn template_version(&self) -> NodeFuture<'_, String> {
        Box::pin(self.release_version(false))
    }
    fn refresh_template(&self) -> NodeFuture<'_, String> {
        Box::pin(self.release_version(true))
    }
    fn prepare<'a>(&'a self, plan: &'a JobPlan) -> NodeFuture<'a, ()> {
        Box::pin(self.unit(plan.slice.job_id, |key| Request::Prepare {
            key,
            plan: plan.clone(),
        }))
    }
    fn prepare_workspace<'a>(&'a self, job: &'a Job) -> NodeFuture<'a, ()> {
        Box::pin(self.unit(job.id as u64, |key| Request::Workspace {
            key,
            job: job.clone(),
        }))
    }
    fn bind_workspace(
        &self,
        id: u64,
        execution: runwell_workspace::Execution,
    ) -> NodeFuture<'_, ()> {
        Box::pin(self.unit(id, |key| Request::Bind { key, execution }))
    }
    fn harvest_workspace<'a>(&'a self, job: &'a Job) -> NodeFuture<'a, ()> {
        Box::pin(self.unit(job.id as u64, |key| Request::Harvest {
            key,
            job: job.clone(),
        }))
    }
    fn reconcile_workspaces<'a>(&'a self, _retained: &'a HashSet<u64>) -> NodeFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn start<'a>(&'a self, plan: &'a JobPlan, launch: &'a LaunchSpec) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            self.unit(plan.slice.job_id, |key| Request::Start {
                key,
                agent_id: launch.agent_id,
                jit: launch.jit_config.expose_secret().to_owned(),
            })
            .await?;
            self.store
                .execution_started(plan.slice.job_id as i64, self.clock.now_ms())
                .await?;
            Ok(())
        })
    }
    fn inspect(&self, id: u64) -> NodeFuture<'_, ProcessState> {
        Box::pin(async move {
            match self.rpc(id, Request::Inspect).await? {
                Response::Process(state) => Ok(state),
                _ => Err(Error::Config),
            }
        })
    }
    fn measure(&self, id: u64) -> NodeFuture<'_, JobMeasurement> {
        Box::pin(async move {
            match self.rpc(id, Request::Measure).await? {
                Response::Measurement(sample) => Ok(sample),
                _ => Err(Error::Config),
            }
        })
    }
    fn stop_runner(&self, id: u64) -> NodeFuture<'_, ()> {
        Box::pin(self.unit(id, Request::Stop))
    }
    fn cleanup(&self, id: u64) -> NodeFuture<'_, ()> {
        Box::pin(self.unit(id, Request::Cleanup))
    }
    fn finished<'a>(&'a self, job: &'a Job, sample: &'a JobMeasurement) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            if job.state != runwell_store::State::Completed {
                let (_, key) = self.route(job.id as u64).await?;
                let reason = if sample.oom_kills > 0 {
                    "oom"
                } else if sample.infra_signal {
                    "node_infrastructure"
                } else if job.outcome.is_some() {
                    "workflow_failure"
                } else if sample.exit_code.is_none() {
                    "execution_missing"
                } else {
                    "runner_exit"
                };
                self.store
                    .fail_attempt(runwell_store::FailureEvent {
                        job_id: job.id,
                        attempt: key.attempt,
                        reason: reason.into(),
                    })
                    .await?;
            }
            self.hooks.completed(job, sample);
            Ok(())
        })
    }
    fn inventory(&self) -> NodeFuture<'_, Vec<LocalJob>> {
        // Each report is reconciled with placement ownership in maintain(). An
        // unreachable node is never represented as an empty host inventory.
        Box::pin(async { Ok(Vec::new()) })
    }
}
