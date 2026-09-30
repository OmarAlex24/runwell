mod common;
use common::*;
use runwell_scaleset::{ActionsClient, Error, JitSettings, ScaleSet};
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};

#[tokio::test]
async fn idempotent_429_and_5xx_retry_with_jitter_and_preserve_final_status() {
    for status in [429, 500, 502, 503, 504] {
        let (server, client, clock) = harness().await;
        Mock::given(method("GET"))
            .and(path(format!("{SETS}/42")))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("ActivityId", "activity-123")
                    .insert_header("X-GitHub-Request-Id", "request-123")
                    .set_body_json(
                        json!({"typeName":"SyntheticException","message":"synthetic-pat"}),
                    ),
            )
            .expect(5)
            .mount(&server)
            .await;
        let error = client.get_scale_set(42).await.unwrap_err();
        assert_eq!(error.status().unwrap().as_u16(), status);
        assert!(error.is_type("SyntheticException"));
        assert!(!format!("{error:?}").contains("synthetic-pat"));
        let Error::Http {
            endpoint,
            activity_id,
            request_id,
            ..
        } = error
        else {
            panic!("HTTP error expected")
        };
        assert_eq!(endpoint, format!("{SETS}/42"));
        assert_eq!(activity_id.as_deref(), Some("activity-123"));
        assert_eq!(request_id.as_deref(), Some("request-123"));
        let sleeps = clock.sleeps.lock().unwrap();
        assert_eq!(sleeps.len(), 4);
        for (attempt, delay) in sleeps.iter().enumerate() {
            let cap = Duration::from_secs(1 << attempt);
            assert!(*delay >= cap / 2 && *delay <= cap);
        }
    }
}

#[tokio::test]
async fn non_idempotent_posts_never_retry_429_or_5xx() {
    for status in [429, 500, 503] {
        let (server, client, clock) = harness().await;
        for endpoint in [
            SETS.to_string(),
            format!("{SETS}/42/generatejitconfig"),
            SESSIONS.into(),
        ] {
            Mock::given(method("POST"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
        }
        assert_eq!(
            client
                .create_scale_set(ScaleSet {
                    name: "test-set".into(),
                    ..Default::default()
                })
                .await
                .unwrap_err()
                .status()
                .unwrap()
                .as_u16(),
            status
        );
        assert_eq!(
            client
                .generate_jit_config(
                    42,
                    &JitSettings {
                        name: "test-runner".into(),
                        work_folder: String::new()
                    }
                )
                .await
                .unwrap_err()
                .status()
                .unwrap()
                .as_u16(),
            status
        );
        let error = match client.open_session(42, "test-owner").await {
            Ok(_) => panic!("unexpected session"),
            Err(error) => error,
        };
        assert_eq!(error.status().unwrap().as_u16(), status);
        assert!(clock.sleeps.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn unsafe_post_transport_timeout_is_not_retried() {
    let server = MockServer::start().await;
    auth(&server).await;
    let mut cfg = config(&server, Arc::new(FakeClock::default()));
    cfg.request_timeout = Duration::from_millis(50);
    let client = ActionsClient::new(cfg).unwrap();
    Mock::given(method("POST"))
        .and(path(format!("{SETS}/42/generatejitconfig")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(fixture("jit"))
                .set_delay(Duration::from_millis(200)),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert!(matches!(
        client
            .generate_jit_config(
                42,
                &JitSettings {
                    name: "test-runner".into(),
                    work_folder: String::new()
                }
            )
            .await,
        Err(Error::Transport { .. })
    ));
}

#[tokio::test]
async fn retry_after_seconds_and_http_date_are_capped() {
    for value in [
        "9999".to_string(),
        httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(NOW + 9999)),
    ] {
        let (server, client, clock) = harness().await;
        Mock::given(method("GET"))
            .and(path(format!("{SETS}/42")))
            .respond_with(Sequence::new(vec![
                ResponseTemplate::new(429).insert_header("Retry-After", value),
                ResponseTemplate::new(200).set_body_json(fixture("set")),
            ]))
            .expect(2)
            .mount(&server)
            .await;
        client.get_scale_set(42).await.unwrap();
        assert_eq!(
            *clock.sleeps.lock().unwrap(),
            vec![Duration::from_secs(300)]
        );
    }
}

#[tokio::test]
async fn permanent_statuses_and_501_are_not_retried() {
    for status in [400, 403, 404, 409, 501] {
        let (server, client, clock) = harness().await;
        Mock::given(method("GET"))
            .and(path(format!("{SETS}/42")))
            .respond_with(ResponseTemplate::new(status).set_body_string("plain error"))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            client
                .get_scale_set(42)
                .await
                .unwrap_err()
                .status()
                .unwrap()
                .as_u16(),
            status
        );
        assert!(clock.sleeps.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn acquire_post_is_explicitly_idempotent_and_removal_retries() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{SETS}/42/acquirejobs")))
        .and(header("Authorization", "Bearer synthetic-queue-token"))
        .and(body_json(json!([1])))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(503),
            ResponseTemplate::new(200).set_body_json(json!({"count":1,"value":[1]})),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{AGENTS}/1234")))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(500),
            ResponseTemplate::new(204),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    let session = client.open_session(42, "test-owner").await.unwrap();
    assert_eq!(session.acquire_jobs(&[1]).await.unwrap(), vec![1]);
    client.remove_runner(1234).await.unwrap();
    session.close().await.unwrap();
}

#[tokio::test]
async fn token_minting_and_registration_5xx_are_not_retried() {
    for endpoint in [
        "/repos/example-org/example-repo/actions/runners/registration-token",
        "/actions/runner-registration",
    ] {
        let (server, client, clock) = harness().await;
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(503))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            client
                .get_scale_set(42)
                .await
                .unwrap_err()
                .status()
                .unwrap()
                .as_u16(),
            503
        );
        assert!(clock.sleeps.lock().unwrap().is_empty());
    }
}
