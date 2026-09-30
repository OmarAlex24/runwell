mod common;
use common::*;
use futures_util::StreamExt;
use runwell_scaleset::{ActionsClient, Error, Listener};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};

#[tokio::test]
async fn queue_401_refreshes_url_and_token_then_retries_once() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    Mock::given(method("GET"))
        .and(path("/queue"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let mut renewed = session(&server);
    renewed["messageQueueUrl"] = json!(format!("{}/renewed?partition=next", server.uri()));
    renewed["messageQueueAccessToken"] = json!("renewed-token");
    Mock::given(method("PATCH"))
        .and(path(SESSION))
        .and(header(
            "Authorization",
            format!("Bearer {}", jwt(NOW + 3600)),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(renewed))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/renewed"))
        .and(header("Authorization", "Bearer renewed-token"))
        .and(query_param("partition", "next"))
        .and(query_param("lastMessageId", "0"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 10);
    listener.next().await.unwrap().unwrap();
    assert_eq!(
        listener
            .next()
            .await
            .unwrap()
            .unwrap_err()
            .status()
            .unwrap()
            .as_u16(),
        401
    );
    listener.close().await.unwrap();
    versions(&server).await;
}

#[tokio::test]
async fn queue_404_recreates_session_and_yields_initial_statistics() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    Mock::given(method("GET"))
        .and(path("/queue"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(404),
            ResponseTemplate::new(200).set_body_json(fixture("message")),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 10);
    listener.next().await.unwrap().unwrap();
    let recovery = listener.next().await.unwrap().unwrap();
    assert!(recovery.message_id.is_none());
    assert_eq!(recovery.statistics.unwrap().total_assigned_jobs, 7);
    assert_eq!(count(&server, "POST", SESSIONS).await, 2);
    assert_eq!(listener.next().await.unwrap().unwrap().message_id, Some(0));
    listener.close().await.unwrap();
}

#[tokio::test]
async fn patch_404_recreates_and_old_message_cannot_be_acked() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    Mock::given(method("GET"))
        .and(path("/queue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("message")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{SETS}/42/acquirejobs")))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(SESSION))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 10);
    listener.next().await.unwrap().unwrap();
    listener.next().await.unwrap().unwrap();
    assert!(matches!(
        listener.acquire_jobs(&[987654321]).await,
        Err(Error::SessionRecreated)
    ));
    assert!(matches!(
        listener.ack(0).await,
        Err(Error::SessionRecreated)
    ));
    assert_eq!(count(&server, "DELETE", "/queue/0").await, 0);
    assert!(listener.next().await.unwrap().unwrap().message_id.is_none());
    listener.close().await.unwrap();
}

#[tokio::test]
async fn session_409_retries_with_backoff_and_preserves_exhausted_status() {
    let server = MockServer::start().await;
    let clock = Arc::new(FakeClock::default());
    auth(&server).await;
    let mut cfg = config(&server, clock.clone());
    cfg.session_conflict_timeout = Duration::from_secs(2);
    let client = ActionsClient::new(cfg).unwrap();
    Mock::given(method("POST"))
        .and(path(SESSIONS))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({"typeName":"TaskAgentSessionConflictException"})),
        )
        .mount(&server)
        .await;
    let error = match client.open_session(42, "test-owner").await {
        Ok(_) => panic!("expected conflict"),
        Err(e) => e,
    };
    assert_eq!(error.status().unwrap().as_u16(), 409);
    assert!(error.is_type("TaskAgentSessionConflictException"));
    let sleeps = clock.sleeps.lock().unwrap();
    assert!(sleeps.len() >= 2);
    assert_eq!(sleeps.iter().sum::<Duration>(), Duration::from_secs(2));
}

#[tokio::test]
async fn session_conflict_then_success_and_explicit_close() {
    let (server, client, clock) = harness().await;
    Mock::given(method("POST"))
        .and(path(SESSIONS))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(409),
            ResponseTemplate::new(200).set_body_json(session(&server)),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(SESSION))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    client
        .open_session(42, "test-owner")
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    assert_eq!(clock.sleeps.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn missing_initial_statistics_deletes_session_then_retries() {
    let (server, client, _) = harness().await;
    let mut incomplete = session(&server);
    incomplete["statistics"] = json!(null);
    Mock::given(method("POST"))
        .and(path(SESSIONS))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(200).set_body_json(incomplete),
            ResponseTemplate::new(200).set_body_json(session(&server)),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(SESSION))
        .respond_with(ResponseTemplate::new(204))
        .expect(2)
        .mount(&server)
        .await;
    let session = client.open_session(42, "test-owner").await.unwrap();
    assert_eq!(session.statistics().await.total_assigned_jobs, 7);
    session.close().await.unwrap();
}

#[tokio::test]
async fn acquire_and_ack_refresh_queue_token_without_implicit_ack() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    Mock::given(method("GET"))
        .and(path("/queue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("message")))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(SESSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(session(&server)))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{SETS}/42/acquirejobs")))
        .and(header("Authorization", "Bearer synthetic-queue-token"))
        .and(body_json(json!([987654321, 999])))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(401),
            ResponseTemplate::new(200).set_body_json(json!({"count":1,"value":[987654321]})),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/queue/0"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(401),
            ResponseTemplate::new(204),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 1);
    listener.next().await.unwrap().unwrap();
    listener.next().await.unwrap().unwrap();
    assert_eq!(
        listener.acquire_jobs(&[987654321, 999]).await.unwrap(),
        vec![987654321]
    );
    assert_eq!(count(&server, "DELETE", "/queue/0").await, 0);
    listener.ack(0).await.unwrap();
    listener.close().await.unwrap();
}

#[tokio::test]
async fn concurrent_queue_401s_share_one_refresh() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    let mut renewed = session(&server);
    renewed["messageQueueAccessToken"] = json!("renewed");
    Mock::given(method("PATCH"))
        .and(path(SESSION))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(renewed)
                .set_delay(Duration::from_millis(30)),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{SETS}/42/acquirejobs")))
        .and(header("Authorization", "Bearer synthetic-queue-token"))
        .respond_with(ResponseTemplate::new(401))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{SETS}/42/acquirejobs")))
        .and(header("Authorization", "Bearer renewed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"count":0,"value":[]})))
        .expect(2)
        .mount(&server)
        .await;
    let session = client.open_session(42, "test-owner").await.unwrap();
    let (a, b) = tokio::join!(session.acquire_jobs(&[1]), session.acquire_jobs(&[2]));
    a.unwrap();
    b.unwrap();
    session.close().await.unwrap();
}

#[tokio::test]
async fn explicit_refresh_recreation_resets_last_message_cursor() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    let mut message = fixture("message");
    message["messageId"] = json!(50);
    Mock::given(method("GET"))
        .and(path("/queue"))
        .and(query_param("lastMessageId", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/queue/50"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(SESSION))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 10);
    listener.next().await.unwrap().unwrap();
    listener.next().await.unwrap().unwrap();
    listener.ack(50).await.unwrap();
    listener.session().refresh().await.unwrap();
    assert!(listener.next().await.unwrap().unwrap().message_id.is_none());
    assert_eq!(listener.next().await.unwrap().unwrap().message_id, Some(50));
    listener.close().await.unwrap();
}
