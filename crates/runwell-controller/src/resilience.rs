use crate::Fleet;
use runwell_node::{Error, ProcessState};
use runwell_store::{FailureEvent, State};
use runwell_transport::{Request, Response};

impl Fleet {
    pub(crate) async fn poll(&self) -> Result<(), Error> {
        let mut requests = tokio::task::JoinSet::new();
        for (id, peer) in &self.peers {
            let (id, peer) = (id.clone(), peer.clone());
            requests.spawn(async move { (id, peer.call(Request::Report).await) });
        }
        while let Some(result) = requests.join_next().await {
            if let Ok((id, Ok(Response::Report(report)))) = result {
                self.accept_report(&id, report).await?;
            }
        }
        Ok(())
    }
    pub(crate) async fn maintain(&self) -> Result<(), Error> {
        self.poll().await?;
        let reports = self.reports().await?;
        let now = self.clock.now_ms();
        let settings = self.config.network.as_ref().ok_or(Error::Config)?;
        self.store
            .mark_lost_nodes(now.saturating_sub(settings.lost_seconds as i64 * 1000))
            .await?;
        for placement in self.store.placements().await? {
            let job = self.store.job(placement.job_id).await?;
            let runner = self.store.runner(job.id).await?;
            if let Some(sample) = self.store.measurement(job.id).await?
                && (sample.oom_kills > 0 || sample.infra_signal)
            {
                self.store
                    .fail_attempt(FailureEvent {
                        job_id: job.id,
                        attempt: placement.attempt,
                        reason: if sample.oom_kills > 0 {
                            "oom"
                        } else {
                            "node_infrastructure"
                        }
                        .into(),
                    })
                    .await?;
            }
            if runner.as_ref().is_some_and(|r| r.cleaned) {
                continue;
            }
            let report = reports.iter().find(|(id, _, _)| id == &placement.node_id);
            let last_seen = report.map_or(placement.assigned_at, |(_, seen, _)| *seen);
            let status = report.and_then(|(_, _, r)| {
                r.jobs
                    .iter()
                    .find(|s| s.key.job_id == job.id && s.key.attempt == placement.attempt)
            });
            // Cancellation can happen before a runner intent exists. Reports
            // include bare admissions, so terminal placements must release those
            // leases even when neither node loss nor failure evidence is present.
            if job.state.terminal() {
                if status.is_some() || placement.lost {
                    self.fence(job.id).await?;
                }
                continue;
            }
            let running = matches!(job.state, State::RunnerCreated | State::Running);
            let executing = running && status.is_some_and(|s| s.process == ProcessState::Running);
            let heartbeat = report
                .and_then(|(_, received, r)| {
                    status.and_then(|s| s.heartbeat_at_ms).map(|at| {
                        received.saturating_sub(r.observed_at_ms.saturating_sub(at).max(0))
                    })
                })
                .or(placement.execution_started_at);
            let watchdog_ms =
                self.watchdog_seconds(&job).await? * f64::from(settings.watchdog_multiple) * 1000.0;
            let reason = if now.saturating_sub(last_seen) >= settings.lost_seconds as i64 * 1000 {
                Some("node_lost")
            } else if executing
                && heartbeat.is_some_and(|at| {
                    now.saturating_sub(at) >= settings.heartbeat_seconds as i64 * 1000
                })
            {
                Some("heartbeat_missing")
            } else if executing
                && placement
                    .execution_started_at
                    .is_some_and(|at| now.saturating_sub(at) as f64 > watchdog_ms)
            {
                Some("duration_watchdog")
            } else if placement.execution_started_at.is_none()
                && now.saturating_sub(placement.assigned_at)
                    > settings.preparation_seconds as i64 * 1000
            {
                Some("preparation_timeout")
            } else if running
                && status.is_none_or(|s| s.process == ProcessState::Absent)
                && report.is_some()
            {
                Some("execution_missing")
            } else {
                None
            };
            if let Some(reason) = reason {
                self.store
                    .fail_attempt(FailureEvent {
                        job_id: job.id,
                        attempt: placement.attempt,
                        reason: reason.into(),
                    })
                    .await?;
            }
            if placement.lost || reason.is_some() {
                self.fence(job.id).await?;
            } else if running
                && let Some(status) = status
                && matches!(status.process, ProcessState::Exited(Some(code)) if code != 0)
            {
                // Exit is only evidence. OOM counters are read after DELETE and
                // passed to M5a; ordinary workflow failure is never auto-retried.
                tracing::debug!(job_id = job.id, "runner exited; awaiting measurements");
            }
        }
        self.orphans(&reports).await?;
        for event in self.store.failures().await? {
            self.hooks.failure(&event).await?;
            self.store
                .failure_delivered(event.job_id, event.attempt)
                .await?;
        }
        Ok(())
    }
    async fn fence(&self, id: i64) -> Result<(), Error> {
        let job = self.store.job(id).await?;
        if !job.state.terminal() {
            self.store.transition(id, State::Orphaned).await?;
        }
        let Some(runner) = self.store.runner(id).await? else {
            // Admission can precede runner intent. Clean a reachable reservation
            // directly; no registration could have been created without intent.
            if let Ok((peer, key)) = self.route(id as u64).await
                && peer.call(Request::Stop(key)).await.is_ok()
                && peer.call(Request::Measure(key)).await.is_ok()
            {
                let _ = peer.call(Request::Cleanup(key)).await;
            }
            return Ok(());
        };
        if !runner.remote_deleted {
            let agent = if let Some(id) = runner.agent_id {
                Some(id)
            } else {
                self.api.lookup(&runner.name).await?.map(|r| r.id)
            };
            if let Some(agent) = agent
                && !self.api.delete(agent).await?
            {
                return Ok(());
            }
            self.store.remote_deleted(id).await?;
        }
        // The existing lifecycle monitors remote_deleted and orders stop ->
        // measurement -> cleanup. If unreachable, all work remains journaled.
        Ok(())
    }
}
