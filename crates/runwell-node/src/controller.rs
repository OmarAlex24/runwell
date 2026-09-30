use crate::{Error, JobPlan, NodeBackend, RunnerApi, SliceSpec};
use runwell_admission::{
    Brake, HostAdmission, PsiBrake, ReservationAdmission, Resources, Threshold,
};
use runwell_config::{Config, JobClass, StandaloneConfig};
use runwell_scaleset::{Event, Job as GithubJob, Message, Statistics};
use runwell_store::{Execution, NewJob, Runner, State, Store};
use std::{collections::BTreeMap, sync::Arc};

/// Durable standalone controller; no systemd or filesystem details live here.
/// A single loop serializes admission decisions and event handling.
pub struct Controller {
    /// Durable lifecycle source of truth.
    pub store: Store,
    pub(crate) backend: Arc<dyn NodeBackend>,
    pub(crate) api: Arc<dyn RunnerApi>,
    pub(crate) classes: BTreeMap<i64, JobClass>,
    pub(crate) settings: StandaloneConfig,
    pub(crate) prefix: String,
    pub(crate) admission: HostAdmission,
    pub(crate) brake: PsiBrake,
    pub(crate) draining: bool,
    pub(crate) outdated: Option<String>,
}
impl Controller {
    /// Wire controller logic to local or remote host/GitHub ports.
    pub fn new(
        config: &Config,
        classes: BTreeMap<i64, JobClass>,
        store: Store,
        backend: Arc<dyn NodeBackend>,
        api: Arc<dyn RunnerApi>,
    ) -> Result<Self, Error> {
        config.validate().map_err(|_| Error::Config)?;
        let settings = config.standalone.clone().ok_or(Error::Config)?;
        let psi = &config.node.psi;
        let shared = Threshold {
            high: psi.pause_percent,
            low: psi.resume_percent,
        };
        let threshold = |value: &Option<runwell_config::PressureThreshold>| {
            value.as_ref().map_or(shared, |v| Threshold {
                high: v.high,
                low: v.low,
            })
        };
        let brake = PsiBrake::new(
            [threshold(&psi.cpu), shared, threshold(&psi.io)],
            psi.dwell_seconds.saturating_mul(1000),
        )
        .map_err(|_| Error::Config)?;
        let policy = ReservationAdmission::new(settings.overcommit.cpu, settings.overcommit.memory)
            .map_err(|_| Error::Config)?;
        let admission = HostAdmission::new(
            Resources {
                cpu_slots: config.node.cpu_slots,
                memory_bytes: config.node.memory_bytes,
            },
            policy,
            settings.max_jobs,
        );
        Ok(Self {
            store,
            backend,
            api,
            classes,
            settings,
            prefix: format!("rw-{}-j", config.node.id),
            admission,
            brake,
            draining: false,
            outdated: None,
        })
    }
    /// Stop new admissions. Existing units and reservations remain intact.
    pub fn drain(&mut self) {
        self.draining = true;
    }
    /// Whether admission was stopped by a shutdown signal.
    pub fn is_draining(&self) -> bool {
        self.draining
    }
    /// True once every locally held reservation has finished cleanup.
    pub async fn is_idle(&self) -> Result<bool, Error> {
        Ok(!self.store.runners().await?.iter().any(|r| !r.cleaned)
            && !self
                .store
                .jobs()
                .await?
                .iter()
                .any(|j| j.state == State::Admitted))
    }
    pub(crate) async fn pressure(&mut self, now: u64) -> Result<Brake, Error> {
        let sample = self.backend.pressure().await?;
        let decision = self.brake.update(now, sample);
        if self.draining || self.outdated.is_some() {
            Ok(Brake::Paused)
        } else {
            Ok(decision)
        }
    }
    /// Realizable class capacity to advertise. Active registrations still consume
    /// slots; paused admission advertises zero, without stopping existing jobs.
    pub async fn capacities(&mut self, now: u64) -> Result<BTreeMap<i64, u32>, Error> {
        let brake = self.pressure(now).await?;
        let jobs = self.store.jobs().await?;
        let runners = self.store.runners().await?;
        Ok(self
            .classes
            .iter()
            .map(|(set, class)| {
                let active = runners
                    .iter()
                    .filter(|r| {
                        !r.cleaned
                            && jobs
                                .iter()
                                .any(|j| j.id == r.job_id && j.metadata.scale_set_id == *set)
                    })
                    .count() as u32;
                let max = if brake == Brake::Paused {
                    0
                } else {
                    active.saturating_add(self.admission.available_jobs(resources(class), brake))
                };
                (*set, max)
            })
            .collect())
    }
    /// Handle every event and absolute demand snapshot before the caller acks.
    pub async fn handle(&mut self, set: i64, message: &Message, now: u64) -> Result<(), Error> {
        for event in &message.events {
            match event {
                Event::JobAvailable(e) => {
                    self.queue(set, &e.job).await?;
                }
                Event::JobAssigned(_) => {}
                Event::JobStarted(e) => {
                    self.bind(set, &e.runner_name, e.runner_id, &e.job, None)
                        .await?;
                }
                Event::JobCompleted(e) => {
                    self.bind(
                        set,
                        &e.runner_name,
                        e.runner_id,
                        &e.job,
                        Some(e.result.clone()),
                    )
                    .await?;
                }
            }
        }
        if let Some(stats) = &message.statistics {
            self.demand(set, stats).await?;
        }
        self.tick(now).await
    }
    async fn queue(&self, set: i64, job: &GithubJob) -> Result<i64, Error> {
        if job.runner_request_id <= 0 {
            return Err(Error::Github);
        }
        let class = self.classes.get(&set).ok_or(Error::Config)?;
        Ok(self
            .store
            .queue(NewJob {
                scale_set_id: set,
                request_id: job.runner_request_id,
                github_job_id: job.job_id.clone(),
                workflow_run_id: job.workflow_run_id,
                repo: format!("{}/{}", job.owner_name, job.repository_name),
                name: job.job_display_name.clone(),
                class: class.name.clone(),
                reserved_cpu: class.cpu_slots,
                reserved_memory: class.memory_high_bytes,
            })
            .await?)
    }
    async fn bind(
        &self,
        set: i64,
        name: &str,
        agent: i64,
        event: &GithubJob,
        outcome: Option<String>,
    ) -> Result<(), Error> {
        let request = event.runner_request_id;
        if request <= 0 {
            return Err(Error::Github);
        }
        let execution = Execution {
            request_id: request,
            github_job_id: event.job_id.clone(),
            workflow_run_id: event.workflow_run_id,
            repo: if event.repository_name.is_empty() {
                String::new()
            } else if event.repository_name.contains('/') {
                event.repository_name.clone()
            } else {
                format!("{}/{}", event.owner_name, event.repository_name)
            },
            name: event.job_display_name.clone(),
        };
        let runners = self.store.runners().await?;
        if let Some(runner) = runners.iter().find(|r| {
            (!name.is_empty() && r.name == name) || (agent > 0 && r.agent_id == Some(agent))
        }) {
            let job = self.store.job(runner.job_id).await?;
            if job.metadata.scale_set_id != set || (agent > 0 && runner.agent_id != Some(agent)) {
                return Err(Error::Github);
            }
            self.store.bind(job.id, execution, outcome).await?;
        } else if outcome.is_some() {
            // A queued job can be canceled before any runner is assigned.
            for job in self.store.jobs().await?.iter().filter(|j| {
                j.metadata.scale_set_id == set
                    && j.metadata.request_id == request
                    && j.state == State::Queued
            }) {
                self.store
                    .bind(job.id, execution.clone(), outcome.clone())
                    .await?;
                self.store.transition(job.id, State::Orphaned).await?;
            }
        }
        Ok(())
    }
    async fn demand(&mut self, set: i64, stats: &Statistics) -> Result<(), Error> {
        let class = self.classes.get(&set).ok_or(Error::Config)?;
        let jobs = self.store.jobs().await?;
        let existing = jobs
            .iter()
            .filter(|j| {
                j.metadata.scale_set_id == set
                    && !j.state.terminal()
                    && (j.acquired || j.metadata.request_id < 0)
            })
            .count() as u32;
        // Acquired jobs lost from a truncated event array need runners too. Bound
        // synthetic intents by max_jobs; subsequent snapshots fill remaining demand.
        let missing = stats
            .total_assigned_jobs
            .saturating_sub(existing)
            .min(self.settings.max_jobs);
        let base = jobs.last().map_or(0, |j| j.id);
        for offset in 1..=missing {
            self.store
                .queue(NewJob {
                    scale_set_id: set,
                    request_id: -(base + i64::from(offset)),
                    github_job_id: String::new(),
                    workflow_run_id: 0,
                    repo: String::new(),
                    name: "assigned demand".into(),
                    class: class.name.clone(),
                    reserved_cpu: class.cpu_slots,
                    reserved_memory: class.memory_high_bytes,
                })
                .await?;
        }
        Ok(())
    }
    pub(crate) fn plan(&self, job: &runwell_store::Job, runner: &Runner) -> Result<JobPlan, Error> {
        let class = self
            .classes
            .get(&job.metadata.scale_set_id)
            .ok_or(Error::Config)?;
        Ok(JobPlan {
            slice: SliceSpec {
                job_id: job.id as u64,
                memory_high: job.metadata.reserved_memory,
                memory_max: class.memory_max_bytes.max(job.metadata.reserved_memory),
                cpu_weight: class.cpu_weight,
                tasks_max: class.tasks_max,
            },
            directory: runner.dir.clone().into(),
            template_version: runner.template_version.clone(),
        })
    }
}
pub(crate) fn resources(class: &JobClass) -> Resources {
    Resources {
        cpu_slots: class.cpu_slots,
        memory_bytes: class.memory_high_bytes,
    }
}
