use runwell_github::{Auth, Error, RestClient};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{header, method, path},
};

fn client(server: &MockServer) -> RestClient {
    RestClient::new(
        server.uri().parse().unwrap(),
        Auth::App {
            app_id: 5,
            installation_id: 99,
            private_key: include_str!("../../runwell-scaleset/tests/fixtures/app-test-key.pem")
                .into(),
        },
    )
    .unwrap()
}
async fn exchange(server: &MockServer, expected: u64) -> Arc<AtomicUsize> {
    let exchanges = Arc::new(AtomicUsize::new(0));
    let count = exchanges.clone();
    Mock::given(method("POST"))
        .and(path("/app/installations/99/access_tokens"))
        .respond_with(move |_: &Request| {
            let n = count.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(201).set_body_json(
                json!({"token":format!("token-{n}"),"expires_at":"2099-01-01T00:00:00Z"}),
            )
        })
        .expect(expected)
        .mount(server)
        .await;
    exchanges
}

#[tokio::test]
async fn concurrent_401s_refresh_once_and_a_late_rejection_keeps_the_new_token() {
    let server = MockServer::start().await;
    let exchanges = exchange(&server, 2).await;
    Mock::given(header("authorization", "Bearer token-0"))
        .respond_with(ResponseTemplate::new(401).set_delay(Duration::from_millis(50)))
        .with_priority(2)
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(header("authorization", "Bearer token-0"))
        .and(path("/late"))
        .respond_with(ResponseTemplate::new(401).set_delay(Duration::from_millis(250)))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(header("authorization", "Bearer token-1"))
        .and(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true})))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(header("authorization", "Bearer token-1"))
        .and(method("POST"))
        .respond_with(ResponseTemplate::new(201))
        .expect(2)
        .mount(&server)
        .await;
    let api = client(&server);
    api.installation_token().await.unwrap();
    let (get, late, post, other_post) = tokio::join!(
        api.get("probe"),
        api.get("late"),
        api.rerun_failed_jobs("a/repo", 42),
        api.rerun_failed_jobs("a/repo", 43)
    );
    assert_eq!(get.unwrap(), json!({"ok":true}));
    assert_eq!(late.unwrap(), json!({"ok":true}));
    post.unwrap();
    other_post.unwrap();
    api.installation_token().await.unwrap();
    assert_eq!(exchanges.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn repeated_401_stops_after_one_replay_and_invalidates_the_second_token() {
    let server = MockServer::start().await;
    let exchanges = exchange(&server, 3).await;
    Mock::given(method("POST"))
        .and(path("/repos/a/repo/actions/runs/42/rerun-failed-jobs"))
        .respond_with(ResponseTemplate::new(401))
        .expect(2)
        .mount(&server)
        .await;
    let api = client(&server);
    assert!(matches!(
        api.rerun_failed_jobs("a/repo", 42).await,
        Err(Error::Status(401))
    ));
    assert_eq!(exchanges.load(Ordering::SeqCst), 2);
    api.installation_token().await.unwrap();
    assert_eq!(exchanges.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn an_ambiguous_post_after_auth_recovery_is_not_replayed_again() {
    let server = MockServer::start().await;
    let exchanges = exchange(&server, 2).await;
    Mock::given(header("authorization", "Bearer token-0"))
        .and(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(header("authorization", "Bearer token-1"))
        .and(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    let api = client(&server);
    assert!(matches!(
        api.rerun_failed_jobs("a/repo", 42).await,
        Err(Error::Status(500))
    ));
    api.installation_token().await.unwrap();
    assert_eq!(exchanges.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_ambiguous_first_post_never_refreshes_or_replays() {
    let server = MockServer::start().await;
    let exchanges = exchange(&server, 1).await;
    Mock::given(header("authorization", "Bearer token-0"))
        .and(method("POST"))
        .respond_with(ResponseTemplate::new(502))
        .expect(1)
        .mount(&server)
        .await;
    let api = client(&server);
    assert!(matches!(
        api.rerun_failed_jobs("a/repo", 42).await,
        Err(Error::Status(502))
    ));
    api.installation_token().await.unwrap();
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);
}
