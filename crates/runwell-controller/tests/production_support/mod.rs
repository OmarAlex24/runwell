#![allow(dead_code)]
use crate::support::Harness;
use runwell_controller::{
    Controller, Fleet, Telemetry,
    execution::{ExecutionResult, ExecutionSource},
};
use runwell_github::{RunJob, RunState};
use runwell_node::{NodeFuture, ProcessState};
use runwell_retry::{RetryApi, RetryFuture};
use runwell_transport::Rpc;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

#[derive(Default)]
pub struct Remote {
    results: Mutex<BTreeMap<String, (String, i64, ExecutionResult)>>,
    pub posts: Mutex<Vec<String>>,
}
impl ExecutionSource for Remote {
    fn execution<'a>(
        &'a self,
        _repo: &'a str,
        run: i64,
        runner: &'a str,
    ) -> NodeFuture<'a, Option<ExecutionResult>> {
        Box::pin(async move {
            let results = self.results.lock().await;
            if results.values().filter(|(_, r, _)| *r == run).count() < 3 {
                return Ok(None);
            }
            Ok(results.get(runner).map(|(_, _, result)| result.clone()))
        })
    }
}
impl RetryApi for Remote {
    fn run_state<'a>(&'a self, _repo: &'a str, run: i64) -> RetryFuture<'a, RunState> {
        Box::pin(async move {
            Ok(RunState {
                id: run,
                run_attempt: 1,
                status: "completed".into(),
            })
        })
    }
    fn attempt_jobs<'a>(
        &'a self,
        repo: &'a str,
        run: i64,
        _attempt: u32,
    ) -> RetryFuture<'a, Vec<RunJob>> {
        Box::pin(async move {
            Ok(self
                .results
                .lock()
                .await
                .values()
                .filter(|(p, r, _)| p == repo && *r == run)
                .map(|(_, _, result)| RunJob {
                    id: result.job_id,
                    status: "completed".into(),
                    conclusion: Some(result.conclusion.clone()),
                })
                .collect())
        })
    }
    fn rerun_failed_jobs<'a>(&'a self, repo: &'a str, _run: i64) -> RetryFuture<'a, ()> {
        Box::pin(async move {
            self.posts.lock().await.push(repo.into());
            Ok(())
        })
    }
}
pub fn make_telemetry(h: &Harness, remote: Arc<Remote>) -> Arc<Telemetry> {
    Arc::new(
        Telemetry::new(
            h.config.clone(),
            h.store.clone(),
            h.clock.clone(),
            remote.clone(),
            remote,
        )
        .unwrap(),
    )
}
pub fn wire(h: &mut Harness, telemetry: Arc<Telemetry>) {
    let peers = h
        .links
        .iter()
        .enumerate()
        .map(|(i, p)| (format!("node-{}", i + 1), p.clone() as Arc<dyn Rpc>))
        .collect();
    let fallback = Arc::new(runwell_scheduler::Runwell {
        priority: runwell_scheduler::Priority::Fifo,
        aging_seconds: 300.0,
        admission: runwell_admission::ReservationAdmission::new(1.0, 1.0).unwrap(),
    });
    h.fleet = Arc::new(
        Fleet::new(
            h.config.clone(),
            h.store.clone(),
            peers,
            h.api.clone(),
            fallback,
            h.clock.clone(),
            telemetry,
        )
        .unwrap()
        .with_production_policy(Default::default()),
    );
    h.controller = Controller::new(
        &h.config,
        BTreeMap::from([(42, h.config.controller.classes[0].clone())]),
        h.store.clone(),
        h.fleet.clone(),
        h.api.clone(),
    )
    .unwrap();
}
pub async fn finish(h: &mut Harness, remote: &Remote, id: i64, infra: bool, code: bool) {
    let job = h.store.job(id).await.unwrap();
    let runner = h.store.runner(id).await.unwrap().unwrap();
    let result = if infra || code { "failure" } else { "success" };
    h.store
        .bind(
            id,
            runwell_store::Execution {
                request_id: job.metadata.request_id,
                github_job_id: job.metadata.github_job_id.clone(),
                workflow_run_id: job.metadata.workflow_run_id,
                repo: job.metadata.repo.clone(),
                name: job.metadata.name.clone(),
            },
            Some(
                if result == "success" {
                    "succeeded"
                } else {
                    "failed"
                }
                .into(),
            ),
        )
        .await
        .unwrap();
    for host in &h.hosts {
        let mut state = host.state.lock().await;
        if let Some(process) = state.processes.get_mut(&(id as u64)) {
            *process = ProcessState::Exited(Some(if infra || code { 1 } else { 0 }));
            state.measurements.insert(
                id as u64,
                runwell_store::JobMeasurement {
                    job_id: id,
                    duration_ms: 3000,
                    oom_kills: u64::from(infra),
                    infra_signal: infra,
                    ..Default::default()
                },
            );
        }
    }
    remote.results.lock().await.insert(
        runner.name,
        (
            job.metadata.repo,
            job.metadata.workflow_run_id,
            ExecutionResult {
                job_id: id * 100,
                attempt: 1,
                conclusion: result.into(),
                code_failure: code,
                annotations: if code {
                    "1 failed, 4 passed".into()
                } else {
                    String::new()
                },
            },
        ),
    );
}
