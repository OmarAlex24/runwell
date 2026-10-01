use runwell_metrics::*;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header_exists, method},
};
fn alert(now: u64) -> Alert {
    Alert {
        schema_version: 1,
        key: "node_missing:n1".into(),
        kind: AlertKind::NodeMissing,
        subject: "n1".into(),
        observed_at: now,
        value: 100.0,
        threshold: 90.0,
    }
}
fn config() -> WebhookConfig {
    WebhookConfig {
        cooldown_seconds: 20,
        initial_backoff_seconds: 2,
        max_backoff_seconds: 10,
        max_attempts: 3,
        ..Default::default()
    }
}
#[tokio::test]
async fn temporary_failure_backs_off_deduplicates_and_then_cools_down() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header_exists("idempotency-key"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let mut hook = Webhook::new(server.uri().parse().unwrap(), config()).unwrap();
    hook.sync(&[alert(0), alert(0)], 0).unwrap();
    assert_eq!(hook.pending(), 1);
    assert_eq!(hook.deliver_due(0, 5).await.deferred, 1);
    assert_eq!(hook.deliver_due(1, 5).await.sent, 0);
    let first = server.received_requests().await.unwrap()[0].clone();
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(204))
        .expect(2)
        .mount(&server)
        .await;
    hook.sync(&[alert(2)], 2).unwrap();
    assert_eq!(hook.deliver_due(2, 5).await.sent, 1);
    let second = server.received_requests().await.unwrap()[0].clone();
    assert_eq!(
        first.headers.get("idempotency-key"),
        second.headers.get("idempotency-key")
    );
    assert_eq!(first.body, second.body);
    hook.sync(&[], 3).unwrap();
    hook.sync(&[alert(4)], 4).unwrap();
    assert_eq!(hook.pending(), 0);
    hook.sync(&[alert(22)], 22).unwrap();
    assert_eq!(hook.deliver_due(22, 5).await.sent, 1);
    let reminders = server.received_requests().await.unwrap();
    assert_ne!(
        reminders[0].headers.get("idempotency-key"),
        reminders[1].headers.get("idempotency-key")
    );
}
#[tokio::test]
async fn permanent_errors_exhaust_and_resolved_conditions_cancel_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400))
        .expect(1)
        .mount(&server)
        .await;
    let mut hook = Webhook::new(server.uri().parse().unwrap(), config()).unwrap();
    hook.sync(&[alert(0)], 0).unwrap();
    assert_eq!(hook.deliver_due(0, 1).await.exhausted, 1);
    hook.sync(&[alert(1)], 1).unwrap();
    assert_eq!(hook.pending(), 0);
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "8"))
        .expect(1)
        .mount(&server)
        .await;
    hook.sync(&[alert(21)], 21).unwrap();
    assert_eq!(hook.deliver_due(21, 1).await.deferred, 1);
    assert_eq!(hook.deliver_due(28, 1).await, DeliveryReport::default());
    hook.sync(&[], 29).unwrap();
    assert_eq!(hook.deliver_due(30, 1).await, DeliveryReport::default());
}
#[tokio::test]
async fn bounded_retry_attempts_and_backoff_do_not_loop_forever() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(3)
        .mount(&server)
        .await;
    let mut hook = Webhook::new(server.uri().parse().unwrap(), config()).unwrap();
    hook.sync(&[alert(0)], 0).unwrap();
    assert_eq!(hook.deliver_due(0, 1).await.deferred, 1);
    assert_eq!(hook.deliver_due(2, 1).await.deferred, 1);
    assert_eq!(hook.deliver_due(5, 1).await, DeliveryReport::default());
    assert_eq!(hook.deliver_due(6, 1).await.exhausted, 1);
    assert_eq!(hook.pending(), 0);
}
