use crate::{
    Telemetry,
    execution::ExecutionResult,
    production::{context, history_key},
};
use runwell_metrics::CompletionKind;
use runwell_node::Error;
use runwell_retry::{FailureClass, FailureEvidence, Outcome, RetryRequest, Signal};
use runwell_store::{CompletedJob, Job, Observation};
use std::collections::BTreeMap;

struct Resolved {
    job: Job,
    result: ExecutionResult,
    evidence: FailureEvidence,
}
impl Telemetry {
    /// Reconcile pending durable hooks. Deferred remote results survive restart.
    /// Call periodically outside the admission loop; retry claims remain at most once.
    pub async fn tick(&self) -> Result<(), Error> {
        let _guard = self.tick_lock.lock().await;
        let now = self.clock.now_ms() / 1000;
        let mut groups: BTreeMap<(String, i64, u32), Vec<Resolved>> = BTreeMap::new();
        for observation in self
            .store
            .observations(now - 3600)
            .await?
            .into_iter()
            .filter(|o| !o.processed)
        {
            let job = self.store.job(observation.job_id).await?;
            // Wait for measurement and the actual JobStarted/Completed binding.
            // Lost hosts may never return; their REST conclusion still supplies
            // authoritative retry identity while history waits for a final sample.
            let Some(runner) = self.store.runner(job.id).await? else {
                continue;
            };
            let result = match self
                .source
                .execution(
                    &job.metadata.repo,
                    job.metadata.workflow_run_id,
                    &runner.name,
                )
                .await
            {
                Ok(Some(result)) => result,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error, "execution observation deferred");
                    continue;
                }
            };
            let evidence = self.evidence(&observation, &result).await?;
            groups
                .entry((
                    job.metadata.repo.to_lowercase(),
                    job.metadata.workflow_run_id,
                    result.attempt,
                ))
                .or_default()
                .push(Resolved {
                    job,
                    result,
                    evidence,
                });
        }
        for ((repo, run_id, attempt), entries) in groups {
            let request = RetryRequest {
                repo,
                run_id,
                attempt,
                evidence: entries
                    .iter()
                    .map(|e| (e.result.job_id, e.evidence.clone()))
                    .collect(),
            };
            // The retry policy checks the complete remote failure set and rejects
            // mixed/unknown failures. Keep deferred runs until all local evidence arrives.
            let outcome = runwell_retry::retry(
                &self.policy,
                &self.classifier,
                &self.store,
                self.api.as_ref(),
                &request,
                now,
            )
            .await;
            match outcome {
                Ok(Outcome::Accepted) => {
                    for entry in &entries {
                        if self.classifier.classify(&entry.evidence).class == FailureClass::Infra {
                            self.metrics.retry(&entry.job.metadata.class);
                        }
                    }
                }
                Ok(Outcome::Stale) => continue,
                Err(error) => {
                    tracing::warn!(%error, "automatic retry deferred or durably suppressed");
                    continue;
                }
                _ => {}
            }
            let vetoed = entries.iter().any(|e| {
                e.result.conclusion != "success"
                    && self.classifier.classify(&e.evidence).class != FailureClass::Infra
            });
            for entry in entries {
                let class = self.classifier.classify(&entry.evidence).class;
                // Preserve incomplete infra evidence for another tick, while
                // recording history/metrics immediately. A known mixed failure
                // set is definitively ineligible and can be acknowledged.
                let processed = !matches!(outcome, Ok(Outcome::Ineligible))
                    || class != FailureClass::Infra
                    || vetoed;
                self.record_result(entry, class, now, processed).await?;
            }
        }
        Ok(())
    }
    async fn evidence(
        &self,
        observation: &Observation,
        result: &ExecutionResult,
    ) -> Result<FailureEvidence, Error> {
        let mut evidence = FailureEvidence {
            conclusion: result.conclusion.clone(),
            annotations_and_tail: result.annotations.clone(),
            ..Default::default()
        };
        if result.code_failure {
            evidence.signals.insert(Signal::CodeFailure);
        }
        let signal = match observation.reason.as_deref() {
            Some("oom") => Some(Signal::OomKill),
            Some("node_lost") => Some(Signal::NodeLost),
            Some("heartbeat_missing" | "execution_missing") => Some(Signal::RunnerLost),
            // A generic backend/exit/watchdog error is not proof of infrastructure failure.
            _ => None,
        };
        if let Some(signal) = signal {
            evidence.signals.insert(signal);
        }
        if self
            .store
            .measurement(observation.job_id)
            .await?
            .is_some_and(|m| m.oom_kills > 0)
        {
            evidence.signals.insert(Signal::OomKill);
        }
        Ok(evidence)
    }
    async fn record_result(
        &self,
        entry: Resolved,
        class: FailureClass,
        now: i64,
        processed: bool,
    ) -> Result<(), Error> {
        let job = &entry.job;
        let metadata = context(&self.store, job).await?;
        let sample = self.store.measurement(job.id).await?;
        let queue_ms = self
            .store
            .placement(job.id)
            .await?
            .and_then(|p| p.execution_started_at)
            .map_or(0, |at| {
                at.saturating_sub(metadata.ready_at_ms).max(0) as u64
            });
        let duration_ms = sample.as_ref().map_or(0, |s| s.duration_ms);
        if sample.is_some() {
            let key = history_key(job, &metadata);
            let criticality = metadata
                .criticality
                .or(self
                    .store
                    .duration_estimate(&key)
                    .await?
                    .map(|e| e.criticality))
                .unwrap_or_default();
            self.store
                .record_completion(CompletedJob {
                    key,
                    github_job_id: entry.result.job_id,
                    run_id: job.metadata.workflow_run_id,
                    attempt: entry.result.attempt,
                    completed_at: now,
                    duration_ms,
                    queue_ms,
                    conclusion: entry.result.conclusion.clone(),
                    criticality,
                })
                .await?;
        }
        let kind = if entry.result.conclusion == "success" {
            CompletionKind::Success
        } else {
            match class {
                FailureClass::Infra => CompletionKind::Infra,
                FailureClass::TestCode => CompletionKind::TestCode,
                FailureClass::Unknown => CompletionKind::Unknown,
            }
        };
        // Store acknowledgment precedes ephemeral metrics: a crash may miss a
        // counter increment, but cannot inflate it by replaying a durable hook.
        self.store
            .observation_result(job.id, class == FailureClass::Infra, processed)
            .await?;
        if self.store.count_observation(job.id).await? {
            self.metrics
                .completed(
                    &job.metadata.class,
                    queue_ms as f64 / 1000.0,
                    duration_ms as f64 / 1000.0,
                    kind,
                )
                .map_err(|_| Error::Config)?;
        }
        Ok(())
    }
}
