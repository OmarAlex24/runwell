use crate::{Controller, Error, ProcessState, service_unit};
use runwell_admission::{Brake, Resources};
use runwell_runner::LaunchSpec;
use runwell_store::{Job, Runner, State};
use std::collections::HashSet;

impl Controller {
    /// Re-adopt survivors before opening sessions. Unknown busy registrations
    /// block admission rather than killing an unaccounted running job.
    pub async fn reconcile(&mut self) -> Result<(), Error> {
        self.backend.initialize().await?;
        let jobs = self.store.jobs().await?;
        let runners = self.store.runners().await?;
        // Exit 7 remains an admission brake across daemon crashes, including
        // after cleanup completed and the failed service no longer exists.
        for runner in &runners {
            if runner.exit_code == Some(7) {
                self.outdated = Some(runner.template_version.clone());
            }
        }
        for job in &jobs {
            if (!job.state.terminal() && job.state != State::Queued)
                || runners.iter().any(|r| r.job_id == job.id && !r.cleaned)
            {
                self.admission.restore(job.id as u64, reservation(job));
            }
        }
        let known: HashSet<_> = runners
            .iter()
            .filter(|r| !r.cleaned)
            .map(|r| r.job_id as u64)
            .collect();
        for local in self.backend.inventory().await? {
            if !known.contains(&local.id) {
                let name = format!("{}{}", self.prefix, local.id);
                if let Some(remote) = self.api.lookup(&name).await? {
                    if remote.name != name
                        || !self.classes.contains_key(&remote.runner_scale_set_id)
                    {
                        return Err(Error::OrphanBusy);
                    }
                    if !self.api.delete(remote.id).await? {
                        return Err(Error::OrphanBusy);
                    }
                }
                self.backend.cleanup(local.id).await?;
            }
        }
        for runner in runners.iter().filter(|r| !r.cleaned) {
            let job = self.store.job(runner.job_id).await?;
            self.validate_identity(runner)?;
            let state = self.backend.inspect(job.id as u64).await?;
            if state == ProcessState::Running {
                self.backend.recover(&self.plan(&job, runner)?).await?;
                if job.state == State::RunnerCreated {
                    self.store.transition(job.id, State::Running).await?;
                } else if job.state != State::Running {
                    return Err(Error::OrphanBusy);
                }
            } else if job.state == State::Admitted {
                // A JIT POST may have succeeded before its response was journaled.
                // Delete that registration by durable name; credentials are not
                // persisted, so it cannot be safely launched after a restart.
                if self.api.lookup(&runner.name).await?.is_some() {
                    self.store.transition(job.id, State::Orphaned).await?;
                    self.finish(&job, runner, state).await?;
                }
            } else {
                self.monitor(&job, runner).await?;
            }
        }
        // Unknown busy registrations above must preserve their mounts as well
        // as their services. Reconcile orphans only after DELETE-first recovery.
        self.backend.reconcile_workspaces(&known).await?;
        Ok(())
    }
    /// Reconsider queued demand, sample exits, and retry retained cleanup work.
    pub async fn tick(&mut self, now: u64) -> Result<(), Error> {
        for runner in self.store.runners().await?.iter().filter(|r| !r.cleaned) {
            let job = self.store.job(runner.job_id).await?;
            if job.state != State::Admitted {
                self.monitor(&job, runner).await?;
            }
        }
        if self.draining {
            return Ok(());
        }
        let mut current = self.backend.template_version().await?;
        if self.outdated.as_ref().is_some_and(|old| *old == current) {
            current = self.backend.refresh_template().await?;
        }
        if self.outdated.as_ref().is_some_and(|old| *old != current) {
            self.outdated = None;
        }

        for job in self.store.jobs().await? {
            if matches!(job.state, State::Queued | State::Admitted) {
                self.progress(job, now).await?;
            }
        }
        Ok(())
    }
    async fn progress(&mut self, mut job: Job, now: u64) -> Result<(), Error> {
        let brake = self.pressure(now).await?;
        if brake != Brake::Open {
            return Ok(());
        }
        if job.state == State::Queued {
            if !self
                .admission
                .reserve(job.id as u64, reservation(&job), brake)
            {
                return Ok(());
            }
            if let Err(error) = self.store.transition(job.id, State::Admitted).await {
                self.admission.release(job.id as u64);
                return Err(error.into());
            }
            job.state = State::Admitted;
        }
        let mut runner = match self.store.runner(job.id).await? {
            Some(runner) => runner,
            None => {
                let version = self.backend.template_version().await?;
                let runner = Runner {
                    job_id: job.id,
                    name: format!("{}{}", self.prefix, job.id),
                    dir: self
                        .settings
                        .runners_dir
                        .join(format!("j{}", job.id))
                        .to_str()
                        .ok_or(Error::Config)?
                        .into(),
                    unit: service_unit(job.id as u64),
                    template_version: version,
                    agent_id: None,
                    exit_code: None,
                    remote_deleted: false,
                    cleaned: false,
                };
                self.store.runner_intent(runner.clone()).await?;
                runner
            }
        };
        let current = self.backend.template_version().await?;
        if runner.template_version != current {
            self.store
                .retarget_template(job.id, current.clone())
                .await?;
            runner.template_version = current;
        }
        self.validate_identity(&runner)?;
        let plan = self.plan(&job, &runner)?;
        self.backend.prepare(&plan).await?;
        self.backend.prepare_workspace(&job).await?;
        // Preparation may be slow. Re-read host PSI before acquiring, then again
        // before JIT. Reservations already include all admitted jobs.
        if self.pressure(now).await? != Brake::Open {
            return Ok(());
        }
        if !job.acquired {
            let acquired = job.metadata.request_id < 0
                || self
                    .api
                    .acquire(job.metadata.scale_set_id, job.metadata.request_id)
                    .await?;
            if !acquired {
                self.store.transition(job.id, State::Orphaned).await?;
                self.finish(&job, &runner, ProcessState::Absent).await?;
                return Ok(());
            }
            self.store.acquired(job.id).await?;
        }
        if self.pressure(now).await? != Brake::Open {
            return Ok(());
        }
        let registration = self
            .api
            .create(job.metadata.scale_set_id, &runner.name)
            .await?;
        self.store.registered(job.id, registration.agent_id).await?;
        let launch = LaunchSpec {
            agent_id: registration.agent_id,
            install_dir: plan.directory.clone(),
            jit_config: registration.jit,
        };
        self.backend.start(&plan, &launch).await?;
        self.store.transition(job.id, State::Running).await?;
        Ok(())
    }
}

fn reservation(job: &Job) -> Resources {
    Resources {
        cpu_slots: job.metadata.reserved_cpu,
        memory_bytes: job.metadata.reserved_memory,
    }
}
