use runwell_node::{Error, NodeFuture, RunnerApi};
use runwell_scheduler::SchedulingPolicy;
use runwell_store::{FailureEvent, Store};
use runwell_transport::{Clock, Handler, Identity, Key, Report, Request, Response, Rpc, RpcFuture};
use std::{collections::BTreeMap, sync::Arc};

/// M5a integration. Failure processing must deduplicate by (job_id, attempt),
/// because a controller can crash between hook success and outbox acknowledgement.
pub trait Hooks: Send + Sync {
    /// Classify/retry hook; reason is evidence, never a workflow result inferred from exit zero.
    fn failure<'a>(&'a self, event: &'a FailureEvent) -> NodeFuture<'a, ()>;
    /// Metrics hook; labels should remain bounded to configured nodes and classes.
    fn report(&self, _report: &Report) {}
    /// M5a duration-learning/terminal-metrics hook. Deduplicate by durable job ID.
    fn completed(&self, _job: &runwell_store::Job, _sample: &runwell_store::JobMeasurement) {}
}
/// Default hook records evidence; M5a supplies classification, retries and metrics.
pub struct LogHooks;
impl Hooks for LogHooks {
    fn failure<'a>(&'a self, event: &'a FailureEvent) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            tracing::warn!(job_id = event.job_id, attempt = event.attempt, reason = %event.reason, "job infrastructure evidence; retry hook pending integration");
            Ok(())
        })
    }
}
/// Multi-host NodeBackend. All placement decisions flow through SchedulingPolicy.
pub struct Fleet {
    pub(crate) releases: Option<tokio::sync::Mutex<runwell_runner::ReleaseClient>>,
    pub(crate) store: Store,
    pub(crate) peers: BTreeMap<String, Arc<dyn Rpc>>,
    pub(crate) api: Arc<dyn RunnerApi>,
    pub(crate) policy: Arc<dyn SchedulingPolicy + Send + Sync>,
    pub(crate) config: runwell_config::Config,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) hooks: Arc<dyn Hooks>,
}
impl Fleet {
    /// Production release selection lives on the controller; tests can stay offline.
    pub fn with_release_updates(mut self) -> Result<Self, Error> {
        self.releases = Some(tokio::sync::Mutex::new(
            runwell_runner::ReleaseClient::new()?
        ));
        Ok(self)
    }

    pub fn new(
        config: runwell_config::Config,
        store: Store,
        peers: BTreeMap<String, Arc<dyn Rpc>>,
        api: Arc<dyn RunnerApi>,
        policy: Arc<dyn SchedulingPolicy + Send + Sync>,
        clock: Arc<dyn Clock>,
        hooks: Arc<dyn Hooks>,
    ) -> Result<Self, Error> {
        let network = config.network.as_ref().ok_or(Error::Config)?;
        if network.nodes.iter().any(|n| !peers.contains_key(&n.id))
            || peers.len() != network.nodes.len()
        {
            return Err(Error::Config);
        }
        Ok(Self {
            releases: None,
            config,
            store,
            peers,
            api,
            policy,
            clock,
            hooks,
        })
    }
    pub(crate) async fn accept_report(&self, id: &str, report: Report) -> Result<(), Error> {
        if report.node_id != id || !self.peers.contains_key(id) || report.sequence <= 0 {
            return Err(Error::Config);
        }
        if self
            .store
            .snapshot(
                id.into(),
                report.sequence,
                self.clock.now_ms(),
                serde_json::to_string(&report).map_err(|_| Error::Config)?,
            )
            .await?
        {
            // Convert node durations to controller time; synchronized wall clocks
            // are not required. First observation survives controller restart.
            for status in &report.jobs {
                if status.phase >= runwell_transport::STARTING
                    && let Some(start) = status.started_at_ms
                    && self
                        .store
                        .placement(status.key.job_id)
                        .await?
                        .is_some_and(|p| p.node_id == id && p.attempt == status.key.attempt)
                {
                    let age = report.observed_at_ms.saturating_sub(start).max(0);
                    self.store
                        .execution_started(
                            status.key.job_id,
                            self.clock.now_ms().saturating_sub(age),
                        )
                        .await?;
                }
            }
            self.hooks.report(&report);
        }
        Ok(())
    }
    pub(crate) async fn reports(&self) -> Result<Vec<(String, i64, Report)>, Error> {
        self.store
            .snapshots()
            .await?
            .into_iter()
            .map(|(id, time, body)| {
                Ok((
                    id,
                    time,
                    serde_json::from_str(&body).map_err(|_| Error::Config)?,
                ))
            })
            .collect()
    }
    pub(crate) async fn route(&self, id: u64) -> Result<(&Arc<dyn Rpc>, Key), Error> {
        let placement = self
            .store
            .placement(id as i64)
            .await?
            .ok_or(Error::Config)?;
        let peer = self.peers.get(&placement.node_id).ok_or(Error::Config)?;
        Ok((
            peer,
            Key {
                job_id: placement.job_id,
                attempt: placement.attempt,
            },
        ))
    }
    pub(crate) async fn rpc(
        &self,
        id: u64,
        request: impl FnOnce(Key) -> Request,
    ) -> Result<Response, Error> {
        let (peer, key) = self.route(id).await?;
        let request = request(key);
        let preparation = matches!(
            request,
            Request::Prepare { .. } | Request::Workspace { .. } | Request::Start { .. }
        );
        match peer.call(request).await {
            Ok(response) => Ok(response),
            Err(runwell_transport::Error::Backend) if preparation => {
                self.store
                    .fail_attempt(FailureEvent {
                        job_id: key.job_id,
                        attempt: key.attempt,
                        reason: "node_operation_failed".into(),
                    })
                    .await?;
                Err(Error::Io)
            }
            Err(_) => Err(Error::Timeout),
        }
    }
    pub(crate) async fn unit(
        &self,
        id: u64,
        request: impl FnOnce(Key) -> Request,
    ) -> Result<(), Error> {
        match self.rpc(id, request).await? {
            Response::Ok => Ok(()),
            _ => Err(Error::Config),
        }
    }
}
impl Handler for Fleet {
    fn handle(&self, peer: Identity, request: Request) -> RpcFuture<'_> {
        Box::pin(async move {
            if peer.role != "node" || !self.peers.contains_key(&peer.id) {
                return Err(runwell_transport::Error::Unauthorized);
            }
            match request {
                Request::Register(report) => {
                    self.accept_report(&peer.id, report).await?;
                    Ok(Response::Ok)
                }
                _ => Err(runwell_transport::Error::Protocol),
            }
        })
    }
}
