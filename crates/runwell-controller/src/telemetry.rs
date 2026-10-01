//! Fast durable hooks. Remote enrichment/retry and webhook I/O run on a separate task.
use crate::{Hooks, execution::ExecutionSource};
use runwell_metrics::{AdmissionDecision, Metrics, PressureResource};
use runwell_node::{Error, NodeFuture};
use runwell_retry::{Classifier, RetryApi, RetryPolicy};
use runwell_store::{FailureEvent, Job, JobMeasurement, Store};
use runwell_transport::{Clock, Report};
use std::sync::Arc;

/// Controller integration for retry safety, learned history, metrics and alerts.
pub struct Telemetry {
    pub(crate) config: runwell_config::Config,
    pub(crate) store: Store,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) api: Arc<dyn RetryApi>,
    pub(crate) source: Arc<dyn ExecutionSource>,
    pub(crate) classifier: Classifier,
    pub(crate) policy: RetryPolicy,
    pub(crate) metrics: Arc<Metrics>,
    pub(crate) started: i64,
    pub(crate) tick_lock: tokio::sync::Mutex<()>,
}
impl Telemetry {
    /// Construct using shared REST ports, the controller clock and durable journal.
    pub fn new(
        config: runwell_config::Config,
        store: Store,
        clock: Arc<dyn Clock>,
        api: Arc<dyn RetryApi>,
        source: Arc<dyn ExecutionSource>,
    ) -> Result<Self, Error> {
        let network = config.network.as_ref().ok_or(Error::Config)?;
        let metrics = Metrics::new(
            config
                .controller
                .classes
                .iter()
                .map(|c| c.name.clone())
                .collect(),
            network.nodes.iter().map(|n| n.id.clone()).collect(),
        )
        .map_err(|_| Error::Config)?;
        let policy = RetryPolicy {
            enabled: config.controller.production.retry_enabled,
            daily_cap: config.controller.production.retry_daily_cap,
        };
        Ok(Self {
            started: clock.now_ms() / 1000,
            config,
            store,
            clock,
            api,
            source,
            classifier: Classifier::builtin().map_err(|_| Error::Config)?,
            policy,
            metrics: Arc::new(metrics),
            tick_lock: tokio::sync::Mutex::new(()),
        })
    }
    /// Shared registry served by the independent HTTP scrape listener.
    pub fn metrics(&self) -> Arc<Metrics> {
        self.metrics.clone()
    }
}
impl Hooks for Telemetry {
    fn failure<'a>(&'a self, event: &'a FailureEvent) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            self.store
                .observe(
                    event.job_id,
                    Some(event.reason.clone()),
                    false,
                    self.clock.now_ms() / 1000,
                )
                .await?;
            Ok(())
        })
    }
    fn completed<'a>(&'a self, job: &'a Job, sample: &'a JobMeasurement) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            self.store
                .observe(
                    job.id,
                    (sample.oom_kills > 0).then(|| "oom".into()),
                    true,
                    self.clock.now_ms() / 1000,
                )
                .await?;
            Ok(())
        })
    }
    fn admission(&self, class: &str, accepted: bool) {
        self.metrics.admission(
            class,
            if accepted {
                AdmissionDecision::Accepted
            } else {
                AdmissionDecision::Capacity
            },
        );
    }
    fn report(&self, report: &Report) {
        let _ = self.metrics.headroom(
            &report.node_id,
            report.cpu_slots.saturating_sub(report.reserved_cpu),
            report.memory_bytes.saturating_sub(report.reserved_memory),
        );
        for (resource, value) in [
            PressureResource::Cpu,
            PressureResource::Memory,
            PressureResource::Io,
        ]
        .into_iter()
        .zip(report.pressure)
        {
            let _ = self.metrics.pressure(&report.node_id, resource, value);
        }
    }
}
