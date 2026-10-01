use crate::*;
use runwell_node::{JobPlan, ProcessState};
use runwell_runner::LaunchSpec;
use secrecy::SecretString;

impl Agent {
    pub(crate) async fn operation(&self, request: Request) -> Result<Response, Error> {
        if let Some(response) = self.cleaned_response(&request).await? {
            return Ok(response);
        }
        match request {
            Request::Orphan(key) => {
                let job_id = key.job_id;
                if job_id <= 0 || key.attempt <= 0 {
                    return Err(Error::Protocol);
                }
                if self
                    .store
                    .lease_record(job_id)
                    .await?
                    .is_some_and(|r| r.attempt != key.attempt)
                {
                    return Err(Error::Protocol);
                }
                let lease = self.store.node_lease(job_id, key.attempt).await?;
                let mut lease = lease.unwrap_or_else(|| orphan_lease(key));
                if lease.attempt != key.attempt {
                    return Err(Error::Protocol);
                }
                if lease.phase != CLEANED {
                    self.backend.stop_runner(job_id as u64).await?;
                    if lease.measurement.is_none() {
                        lease.measurement = Some(self.backend.measure(job_id as u64).await?);
                    }
                    lease.phase = STOPPED;
                    self.store.lease(lease.clone()).await?;
                    self.backend.cleanup(job_id as u64).await?;
                    lease.phase = CLEANED;
                    self.store.lease(lease).await?;
                    self.state.lock().await.host.release(job_id as u64);
                }
            }
            Request::Prepare { key, mut plan } => {
                let mut lease = self.lease(key).await?;
                if lease.phase >= STARTING {
                    return Ok(Response::Ok);
                }
                if plan.slice.job_id != key.job_id as u64
                    || plan.slice.memory_high != lease.job.metadata.reserved_memory
                {
                    return Err(Error::Protocol);
                }
                plan.slice.validate()?;
                let settings = self.config.standalone.as_ref().ok_or(Error::Protocol)?;
                plan.directory = settings.runners_dir.join(format!("j{}", key.job_id));
                if lease.phase >= PREPARED {
                    let mut old: JobPlan =
                        serde_json::from_str(lease.plan.as_deref().ok_or(Error::Protocol)?)
                            .map_err(|_| Error::Protocol)?;
                    let same_version = old.template_version == plan.template_version;
                    old.template_version.clone_from(&plan.template_version);
                    if serde_json::to_vec(&old).map_err(|_| Error::Protocol)?
                        != serde_json::to_vec(&plan).map_err(|_| Error::Protocol)?
                    {
                        return Err(Error::Protocol);
                    }
                    if same_version {
                        return Ok(Response::Ok);
                    }
                    if self.backend.inspect(key.job_id as u64).await? == ProcessState::Running {
                        return Err(Error::Protocol);
                    }
                }
                lease.plan = Some(serde_json::to_string(&plan).map_err(|_| Error::Protocol)?);
                self.store.lease(lease.clone()).await?;
                self.backend.prepare(&plan).await?;
                lease.phase = lease.phase.max(PREPARED);
                self.store.lease(lease).await?;
            }
            Request::Workspace { key, job } => {
                let mut lease = self.lease(key).await?;
                if job.id != key.job_id {
                    return Err(Error::Protocol);
                }
                if lease.phase == PREPARED {
                    self.backend.prepare_workspace(&job).await?;
                    lease.phase = WORKSPACE_READY;
                    self.store.lease(lease).await?;
                }
            }
            Request::Bind { key, execution } => {
                if self.lease(key).await?.phase < CLEANED {
                    self.backend
                        .bind_workspace(key.job_id as u64, execution)
                        .await?;
                }
            }
            Request::Harvest { key, job } => {
                let lease = self.lease(key).await?;
                if job.id != key.job_id {
                    return Err(Error::Protocol);
                }
                if lease.phase == STOPPED && lease.measurement.is_some() {
                    self.backend.harvest_workspace(&job).await?;
                }
            }
            Request::Start { key, agent_id, jit } => {
                let mut lease = self.lease(key).await?;
                // STARTED is durable success even if the process exited before
                // the caller recovered its lost reply. Never inspect or relaunch.
                if lease.phase >= STARTED {
                    return Ok(Response::Ok);
                }
                if lease.phase == STARTING {
                    // Only positive host evidence can resolve an ambiguous start.
                    if self.backend.inspect(key.job_id as u64).await? == ProcessState::Running {
                        lease.phase = STARTED;
                        self.store.lease(lease).await?;
                        return Ok(Response::Ok);
                    }
                    return Err(Error::Uncertain);
                }
                if lease.phase != WORKSPACE_READY || agent_id <= 0 {
                    return Err(Error::Protocol);
                }
                let plan: JobPlan =
                    serde_json::from_str(lease.plan.as_deref().ok_or(Error::Protocol)?)
                        .map_err(|_| Error::Protocol)?;
                lease.phase = STARTING;
                lease.started_at_ms = Some(self.clock.now_ms());
                self.store.lease(lease.clone()).await?;
                self.backend
                    .start(
                        &plan,
                        &LaunchSpec {
                            agent_id,
                            install_dir: plan.directory.clone(),
                            jit_config: SecretString::from(jit),
                        },
                    )
                    .await?;
                lease.phase = STARTED;
                self.store.lease(lease).await?;
            }
            Request::Inspect(key) => {
                self.lease(key).await?;
                return Ok(Response::Process(
                    self.backend.inspect(key.job_id as u64).await?,
                ));
            }
            Request::Measure(key) => {
                let mut lease = self.lease(key).await?;
                if let Some(sample) = lease.measurement {
                    return Ok(Response::Measurement(sample));
                }
                if lease.phase < STOPPED {
                    return Err(Error::Protocol);
                }
                let sample = self.backend.measure(key.job_id as u64).await?;
                lease.measurement = Some(sample.clone());
                self.store.lease(lease).await?;
                return Ok(Response::Measurement(sample));
            }
            Request::Stop(key) => {
                let mut lease = self.lease(key).await?;
                if lease.phase == ADMITTED && lease.plan.is_none() {
                    // No preparation intent means no host side effect could have
                    // happened. A bare reservation has no cgroup to stop/measure.
                    lease.measurement = Some(runwell_store::JobMeasurement {
                        job_id: key.job_id,
                        ..Default::default()
                    });
                    lease.phase = CLEANED;
                    self.store.lease(lease).await?;
                    self.state.lock().await.host.release(key.job_id as u64);
                    return Ok(Response::Ok);
                }
                if lease.phase < STOPPED {
                    self.backend.stop_runner(key.job_id as u64).await?;
                    lease.phase = STOPPED;
                    self.store.lease(lease).await?;
                }
            }
            Request::Cleanup(key) => {
                let mut lease = self.lease(key).await?;
                if lease.phase == CLEANED {
                    return Ok(Response::Ok);
                }
                if lease.phase != STOPPED || lease.measurement.is_none() {
                    return Err(Error::Protocol);
                }
                self.backend.cleanup(key.job_id as u64).await?;
                lease.phase = CLEANED;
                self.store.lease(lease).await?;
                self.state.lock().await.host.release(key.job_id as u64);
            }
            _ => return Err(Error::Protocol),
        }
        Ok(Response::Ok)
    }
}

fn orphan_lease(key: Key) -> runwell_store::NodeLease {
    let id = key.job_id;
    runwell_store::NodeLease {
        job: runwell_store::Job {
            id,
            metadata: runwell_store::NewJob {
                scale_set_id: 0,
                request_id: 0,
                github_job_id: String::new(),
                workflow_run_id: 0,
                repo: String::new(),
                name: String::new(),
                class: String::new(),
                reserved_cpu: 0,
                reserved_memory: 0,
            },
            state: runwell_store::State::Orphaned,
            acquired: false,
            actual_request_id: None,
            outcome: None,
            outcome_at: None,
            started_at: None,
        },
        attempt: key.attempt,
        phase: ADMITTED,
        plan: None,
        measurement: None,
        started_at_ms: None,
        heartbeat_at_ms: None,
    }
}
