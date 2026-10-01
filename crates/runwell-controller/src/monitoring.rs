//! Independent metrics listener and bounded periodic observation/webhook delivery.
use crate::Telemetry;
use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    routing::get,
};
use runwell_metrics::{Metrics, Webhook, WebhookConfig};
use runwell_node::Error;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// Serve OpenMetrics at `/metrics`; callers choose the bind address and shutdown token.
pub async fn serve_metrics(
    listener: tokio::net::TcpListener,
    metrics: Arc<Metrics>,
    stop: CancellationToken,
) -> Result<(), std::io::Error> {
    let app = Router::new()
        .route("/metrics", get(scrape))
        .with_state(metrics);
    axum::serve(listener, app)
        .with_graceful_shutdown(stop.cancelled_owned())
        .await
}
async fn scrape(
    State(metrics): State<Arc<Metrics>>,
) -> Result<([(header::HeaderName, &'static str); 1], String), StatusCode> {
    let body = metrics
        .encode()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((
        [(
            header::CONTENT_TYPE,
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )],
        body,
    ))
}
pub(crate) fn start(
    telemetry: Arc<Telemetry>,
    stop: CancellationToken,
) -> Result<tokio::task::JoinHandle<()>, Error> {
    let settings = &telemetry.config.controller.production;
    let mut webhook = settings
        .alert_webhook
        .as_ref()
        .map(|url| {
            Webhook::new(
                url.parse().map_err(|_| Error::Config)?,
                WebhookConfig::default(),
            )
            .map_err(|_| Error::Config)
        })
        .transpose()?;
    let seconds = settings.tick_seconds;
    Ok(tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut active = BTreeSet::new();
        loop {
            tokio::select! { _ = stop.cancelled() => break, _ = interval.tick() => {} }
            let work = async {
                if let Err(error) = telemetry.tick().await {
                    tracing::warn!(%error, "controller observation tick deferred");
                }
                match telemetry.alerts().await {
                    Ok(alerts) => {
                        let keys: BTreeSet<_> = alerts.iter().map(|a| a.key.clone()).collect();
                        for alert in &alerts {
                            if !active.contains(&alert.key) {
                                tracing::warn!(kind = ?alert.kind, subject = %alert.subject, "controller alert active");
                            }
                        }
                        active = keys;
                        if let Some(webhook) = &mut webhook {
                            let now = (telemetry.clock.now_ms() / 1000).max(0) as u64;
                            if webhook.sync(&alerts, now).is_ok() {
                                webhook.deliver_due(now, 4).await;
                            }
                        }
                    }
                    Err(error) => tracing::warn!(%error, "controller alert evaluation deferred"),
                }
            };
            tokio::select! { _ = stop.cancelled() => break, _ = work => {} }
        }
    }))
}
