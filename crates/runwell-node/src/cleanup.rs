use crate::{Controller, Error, ProcessState, service_unit};
use runwell_runner::{ExitDisposition, classify_exit};
use runwell_store::{Job, Runner, State};

impl Controller {
    pub(crate) async fn monitor(&mut self, job: &Job, runner: &Runner) -> Result<(), Error> {
        let mut state = self.backend.inspect(job.id as u64).await?;
        if state != ProcessState::Absent && job.state == State::RunnerCreated {
            self.store.transition(job.id, State::Running).await?;
        }
        if state == ProcessState::Running {
            let now = jiff::Timestamp::now().as_second();
            let idle = job.actual_request_id.is_none()
                && job
                    .started_at
                    .is_some_and(|t| now - t >= self.settings.idle_seconds as i64);
            let complete = job.outcome_at.is_some_and(|t| now - t >= 120);
            if !runner.remote_deleted && !(idle || complete) {
                return Ok(());
            }
            if !runner.remote_deleted {
                let agent = runner.agent_id.ok_or(Error::OrphanBusy)?;
                if !self.api.delete(agent).await? {
                    return Ok(());
                }
                self.store.remote_deleted(job.id).await?;
            }
            self.backend.stop_runner(job.id as u64).await?;
            state = self.backend.inspect(job.id as u64).await?;
        }
        self.finish(job, runner, state).await
    }
    pub(crate) async fn finish(
        &mut self,
        previous: &Job,
        runner: &Runner,
        state: ProcessState,
    ) -> Result<(), Error> {
        if state == ProcessState::Running {
            return Ok(());
        }
        let runner = self
            .store
            .runner(runner.job_id)
            .await?
            .ok_or(Error::Config)?;
        self.validate_identity(&runner)?;
        let job = self.store.job(previous.id).await?;
        let code = match state {
            ProcessState::Exited(code) => code.or(runner.exit_code),
            _ => runner.exit_code,
        };
        self.store.exited(job.id, code).await?;
        if classify_exit(code) == ExitDisposition::Outdated {
            self.outdated = Some(runner.template_version.clone());
        }
        if !runner.remote_deleted {
            let agent = if let Some(agent) = runner.agent_id {
                Some(agent)
            } else {
                match self.api.lookup(&runner.name).await? {
                    Some(remote)
                        if remote.name == runner.name
                            && remote.runner_scale_set_id == job.metadata.scale_set_id =>
                    {
                        Some(remote.id)
                    }
                    Some(_) => return Err(Error::OrphanBusy),
                    None => None,
                }
            };
            if let Some(agent) = agent
                && !self.api.delete(agent).await?
            {
                return Ok(());
            }
            self.store.remote_deleted(job.id).await?;
        }
        // DELETE has established that no new job can start. Stop remaining
        // descendants while retaining the independent slice for final counters.
        self.backend.stop_runner(job.id as u64).await?;
        if self.store.measurement(job.id).await?.is_none() {
            let mut sample = self.backend.measure(job.id as u64).await?;
            sample.job_id = job.id;
            sample.exit_code = code;
            sample.infra_signal |= sample.oom_kills > 0;
            if sample.duration_ms == 0 {
                sample.duration_ms = job.started_at.map_or(0, |t| {
                    (jiff::Timestamp::now().as_second() - t).max(0) as u64 * 1000
                });
            }
            self.store.record(&sample).await?;
        }
        if !job.state.terminal() {
            let terminal = if let Some(result) = &job.outcome {
                if job.state == State::Running && result.eq_ignore_ascii_case("succeeded") {
                    State::Completed
                } else {
                    State::Failed
                }
            } else if code.is_some_and(|c| c != 0)
                || self
                    .store
                    .measurement(job.id)
                    .await?
                    .is_some_and(|m| m.infra_signal)
            {
                State::Failed
            } else {
                let measured = self.store.measured_at(job.id).await?.unwrap_or(0);
                if code == Some(0)
                    && job.state == State::Running
                    && jiff::Timestamp::now().as_second() - measured < 120
                {
                    return Ok(());
                }
                State::Orphaned
            };
            self.store.transition(job.id, terminal).await?;
        }
        let final_job = self.store.job(job.id).await?;
        let sample = self.store.measurement(job.id).await?.ok_or(Error::Config)?;
        self.backend.finished(&final_job, &sample).await?;
        self.backend.harvest_workspace(&final_job).await?;
        self.backend.cleanup(job.id as u64).await?;
        self.store.cleaned(job.id).await?;
        self.admission.release(job.id as u64);
        Ok(())
    }
    pub(crate) fn validate_identity(&self, runner: &Runner) -> Result<(), Error> {
        if runner.job_id <= 0
            || runner.name != format!("{}{}", self.prefix, runner.job_id)
            || runner.unit != service_unit(runner.job_id as u64)
            || std::path::Path::new(&runner.dir)
                != self
                    .settings
                    .runners_dir
                    .join(format!("j{}", runner.job_id))
        {
            return Err(Error::Config);
        }
        Ok(())
    }
}
