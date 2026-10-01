use crate::{
    Telemetry,
    production::{context, history_key},
};
use runwell_metrics::{
    Alert, AlertConfig, AlertSnapshot, Completion, JobWatch, NodeWatch, TemplateWatch,
};
use runwell_node::Error;
use runwell_transport::Report;

impl Telemetry {
    /// Evaluate current durable fleet state, including configured nodes with no report.
    pub async fn alerts(&self) -> Result<Vec<Alert>, Error> {
        let now = (self.clock.now_ms() / 1000).max(0) as u64;
        let network = self.config.network.as_ref().ok_or(Error::Config)?;
        let config = AlertConfig {
            heartbeat_seconds: network.lost_seconds,
            stuck_p90_multiple: f64::from(network.watchdog_multiple),
            ..Default::default()
        };
        let mut snapshot = AlertSnapshot::default();
        for observation in self
            .store
            .observations(now as i64 - config.window_seconds as i64)
            .await?
        {
            if observation.counted {
                snapshot.completions.push(Completion {
                    id: observation.job_id.to_string(),
                    at: observation.observed_at.max(0) as u64,
                    infra: observation.infra,
                });
            }
        }
        for job in self
            .store
            .jobs()
            .await?
            .into_iter()
            .filter(|j| !j.state.terminal())
        {
            let metadata = context(&self.store, &job).await?;
            let p90 = self
                .store
                .duration_estimate(&history_key(&job, &metadata))
                .await?
                .map_or(network.expected_seconds as f64, |e| e.p90_seconds);
            let start = self
                .store
                .placement(job.id)
                .await?
                .and_then(|p| p.execution_started_at);
            snapshot.jobs.push(JobWatch {
                id: format!(
                    "{}:{}",
                    job.id,
                    if start.is_some() { "running" } else { "queued" }
                ),
                since: (start.unwrap_or(metadata.ready_at_ms) / 1000).max(0) as u64,
                p90_seconds: p90,
            });
        }
        let reports = self.store.snapshots().await?;
        for node in &network.nodes {
            let report = reports.iter().find(|(id, _, _)| id == &node.id);
            snapshot.nodes.push(NodeWatch {
                id: node.id.clone(),
                last_report: report.map_or(self.started, |(_, at, _)| at / 1000).max(0) as u64,
            });
            if let Some((_, _, payload)) = report {
                let report: Report = serde_json::from_str(payload).map_err(|_| Error::Config)?;
                if let Some(expiry) = self.store.template_expiry(&report.template_version).await? {
                    snapshot.templates.push(TemplateWatch {
                        version: report.template_version,
                        expires_at: expiry.max(0) as u64,
                    });
                }
            }
        }
        let (_, ratio) = snapshot.infra_ratio(now, config.window_seconds);
        self.metrics
            .infra_failure_ratio(ratio)
            .map_err(|_| Error::Config)?;
        runwell_metrics::evaluate(&config, &snapshot, now).map_err(|_| Error::Config)
    }
}
