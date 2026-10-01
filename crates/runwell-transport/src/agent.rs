use crate::*;
use runwell_admission::{
    Brake, HostAdmission, Pressure, PsiBrake, ReservationAdmission, Resources, Threshold,
};
use runwell_node::{NodeBackend, ProcessState};
use runwell_store::{NodeLease, Store};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Injectable millisecond clock, shared by deterministic chaos tests.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}
/// Wall clock for deadlines that must survive process restart.
pub struct WallClock;
impl Clock for WallClock {
    fn now_ms(&self) -> i64 {
        jiff::Timestamp::now().as_millisecond()
    }
}

/// Host-owned durable admission and execution state; no GitHub session or token.
pub struct Agent {
    pub(crate) backend: Arc<dyn NodeBackend>,
    pub(crate) store: Store,
    pub(crate) config: runwell_config::Config,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) state: Mutex<Admission>,
    operations: Mutex<()>,
}
pub(crate) struct Admission {
    pub host: HostAdmission,
    pub brake: PsiBrake,
}
impl Agent {
    /// Re-adopt services and rebuild reservations before accepting RPCs.
    pub async fn open(
        config: runwell_config::Config,
        store: Store,
        backend: Arc<dyn NodeBackend>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, Error> {
        let settings = config.standalone.as_ref().ok_or(Error::Protocol)?;
        let psi = &config.node.psi;
        let threshold = |v: &Option<runwell_config::PressureThreshold>| {
            v.as_ref().map_or(
                Threshold {
                    high: psi.pause_percent,
                    low: psi.resume_percent,
                },
                |t| Threshold {
                    high: t.high,
                    low: t.low,
                },
            )
        };
        let brake = PsiBrake::new(
            [threshold(&psi.cpu), threshold(&None), threshold(&psi.io)],
            psi.dwell_seconds * 1000,
        )
        .map_err(|_| Error::Protocol)?;
        let policy = ReservationAdmission::new(settings.overcommit.cpu, settings.overcommit.memory)
            .map_err(|_| Error::Protocol)?;
        let mut host = HostAdmission::new(
            Resources {
                cpu_slots: config.node.cpu_slots,
                memory_bytes: config.node.memory_bytes,
            },
            policy,
            settings.max_jobs,
        );
        backend.initialize().await?;
        let mut retained = std::collections::HashSet::new();
        for mut lease in store.active_leases().await? {
            if lease.phase < CLEANED {
                // Old journals have no execution epoch. Grant one conservative,
                // durable grace period on upgrade rather than using assignment.
                if lease.phase >= STARTING && lease.started_at_ms.is_none() {
                    lease.started_at_ms = Some(clock.now_ms());
                    store.lease(lease.clone()).await?;
                }
                host.restore(lease.job.id as u64, resources(&lease.job));
                retained.insert(lease.job.id as u64);
                if let Some(plan) = &lease.plan {
                    let plan = serde_json::from_str(plan).map_err(|_| Error::Protocol)?;
                    if backend.inspect(lease.job.id as u64).await? == ProcessState::Running {
                        backend.recover(&plan).await?;
                    }
                }
            }
        }
        // Unknown units are retained until controller-authorized orphan cleanup.
        retained.extend(backend.inventory().await?.into_iter().map(|j| j.id));
        backend.reconcile_workspaces(&retained).await?;
        Ok(Self {
            backend,
            store,
            config,
            clock,
            state: Mutex::new(Admission { host, brake }),
            operations: Mutex::new(()),
        })
    }
    pub(crate) async fn lease(&self, key: Key) -> Result<NodeLease, Error> {
        if key.job_id <= 0 || key.attempt <= 0 {
            return Err(Error::Protocol);
        }
        self.store
            .node_lease(key.job_id, key.attempt)
            .await?
            .ok_or(Error::Protocol)
    }
    pub(crate) async fn pressure(&self, state: &mut Admission) -> (Pressure, Brake) {
        let pressure = self.backend.pressure().await.unwrap_or(Pressure {
            cpu: 100.0,
            memory: 100.0,
            io: 100.0,
            memory_full: 100.0,
        });
        let brake = state
            .brake
            .update(self.clock.now_ms().max(0) as u64, pressure);
        (pressure, brake)
    }
    async fn report(&self, state: &mut Admission) -> Result<Response, Error> {
        let (pressure, brake) = self.pressure(state).await;
        let inventory = self.backend.inventory().await?;
        let leases = self.store.active_leases().await?;
        let unknown = inventory.iter().any(|local| {
            !leases
                .iter()
                .any(|l| l.job.id == local.id as i64 && l.phase < CLEANED)
        });
        let draining = self.store.node_draining().await? || unknown;
        let mut jobs = Vec::new();
        for lease in leases {
            if lease.phase < CLEANED {
                let process = self.backend.inspect(lease.job.id as u64).await?;
                let mut heartbeat = lease.heartbeat_at_ms;
                if process == ProcessState::Running
                    && let Ok(Some(at)) = self.backend.runner_heartbeat(lease.job.id as u64).await
                    && lease.started_at_ms.is_some_and(|start| at >= start)
                    && at <= self.clock.now_ms()
                    && heartbeat.is_none_or(|previous| at > previous)
                {
                    self.store
                        .runner_heartbeat(lease.job.id, lease.attempt, at)
                        .await?;
                    heartbeat = Some(at);
                }
                jobs.push(JobStatus {
                    key: Key {
                        job_id: lease.job.id,
                        attempt: lease.attempt,
                    },
                    phase: lease.phase,
                    process,
                    started_at_ms: lease.started_at_ms,
                    heartbeat_at_ms: heartbeat,
                });
            }
        }
        let used = state.host.used();
        let settings = self.config.standalone.as_ref().ok_or(Error::Protocol)?;
        Ok(Response::Report(Report {
            node_id: self.config.node.id.clone(),
            sequence: self.store.next_sequence().await?,
            observed_at_ms: self.clock.now_ms(),
            draining: draining || brake != Brake::Open,
            cpu_slots: self.config.node.cpu_slots,
            memory_bytes: self.config.node.memory_bytes,
            reserved_cpu: used.cpu_slots,
            reserved_memory: used.memory_bytes,
            free_slots: if draining || brake != Brake::Open {
                0
            } else {
                settings.max_jobs.saturating_sub(jobs.len() as u32)
            },
            pressure: [
                pressure.cpu,
                pressure.memory,
                pressure.io,
                pressure.memory_full,
            ],
            template_version: settings.runner.version.clone(),
            jobs,
            inventory,
        }))
    }
    async fn admit(
        &self,
        key: Key,
        job: runwell_store::Job,
        state: &mut Admission,
    ) -> Result<Response, Error> {
        if key.job_id != job.id || key.job_id <= 0 || key.attempt <= 0 {
            return Err(Error::Protocol);
        }
        if let Ok(lease) = self.lease(key).await {
            if lease.job.metadata.reserved_cpu != job.metadata.reserved_cpu
                || lease.job.metadata.reserved_memory != job.metadata.reserved_memory
            {
                return Err(Error::Protocol);
            }
            let (_, brake) = self.pressure(state).await;
            return Ok(Response::Admitted(
                lease.phase < STOPPED && brake == Brake::Open,
            ));
        }
        if let Some(record) = self.store.lease_record(key.job_id).await? {
            return if record.attempt == key.attempt && record.phase == CLEANED {
                Ok(Response::Admitted(false))
            } else {
                Err(Error::Protocol)
            };
        }
        let leases = self.store.active_leases().await?;
        if self.backend.inventory().await?.iter().any(|j| {
            !leases
                .iter()
                .any(|l| l.job.id == j.id as i64 && l.phase < CLEANED)
        }) {
            return Ok(Response::Admitted(false));
        }
        let (_, brake) = self.pressure(state).await;
        if self.store.node_draining().await?
            || !state.host.reserve(job.id as u64, resources(&job), brake)
        {
            return Ok(Response::Admitted(false));
        }
        let result = self
            .store
            .lease(NodeLease {
                job,
                attempt: key.attempt,
                phase: ADMITTED,
                plan: None,
                measurement: None,
                started_at_ms: None,
                heartbeat_at_ms: None,
            })
            .await;
        if result.is_err() {
            state.host.release(key.job_id as u64);
        }
        result?;
        Ok(Response::Admitted(true))
    }
}
impl Rpc for Agent {
    fn call(&self, request: Request) -> RpcFuture<'_> {
        Box::pin(async move {
            if matches!(request, Request::Report) {
                // Slow template/workspace preparation must not starve liveness
                // reports. Only admission accounting uses this short-lived lock.
                return self.report(&mut *self.state.lock().await).await;
            }
            let _operation = self.operations.lock().await;
            match request {
                Request::Register(_) | Request::Report => Err(Error::Protocol),
                Request::Drain => {
                    self.store.drain_node().await?;
                    Ok(Response::Ok)
                }
                Request::Admit { key, job } => {
                    self.admit(key, job, &mut *self.state.lock().await).await
                }
                request => self.operation(request).await,
            }
        })
    }
}
impl Handler for Agent {
    fn handle(&self, peer: Identity, request: Request) -> RpcFuture<'_> {
        Box::pin(async move {
            let expected = self
                .config
                .network
                .as_ref()
                .ok_or(Error::Protocol)?
                .controller_id
                .as_str();
            if peer != Identity::controller(expected) {
                return Err(Error::Unauthorized);
            }
            self.call(request).await
        })
    }
}
fn resources(job: &runwell_store::Job) -> Resources {
    Resources {
        cpu_slots: job.metadata.reserved_cpu,
        memory_bytes: job.metadata.reserved_memory,
    }
}
