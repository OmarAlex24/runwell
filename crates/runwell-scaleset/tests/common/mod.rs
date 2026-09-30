#![allow(dead_code)]
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runwell_scaleset::{ActionsClient, Config, Credentials, Secret, retry::Clock};
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::*};

pub const SETS: &str = "/_apis/runtime/runnerscalesets";
pub const SESSIONS: &str = "/_apis/runtime/runnerscalesets/42/sessions";
pub const SESSION: &str =
    "/_apis/runtime/runnerscalesets/42/sessions/11111111-1111-4111-8111-111111111111";
pub const AGENTS: &str = "/_apis/distributedtask/pools/0/agents";
pub const NOW: u64 = 1_800_000_000;

#[derive(Default)]
pub struct FakeClock {
    millis: AtomicU64,
    pub sleeps: Mutex<Vec<Duration>>,
}
impl FakeClock {
    pub fn advance(&self, duration: Duration) {
        self.millis
            .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
    }
}
impl Clock for FakeClock {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH
            + Duration::from_secs(NOW)
            + Duration::from_millis(self.millis.load(Ordering::SeqCst))
    }
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.sleeps.lock().unwrap().push(duration);
            self.advance(duration);
            tokio::task::yield_now().await;
        })
    }
}

pub struct Sequence {
    responses: Vec<ResponseTemplate>,
    next: AtomicUsize,
}
impl Sequence {
    pub fn new(responses: Vec<ResponseTemplate>) -> Self {
        Self {
            responses,
            next: AtomicUsize::new(0),
        }
    }
}
impl Respond for Sequence {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let i = self
            .next
            .fetch_add(1, Ordering::SeqCst)
            .min(self.responses.len() - 1);
        self.responses[i].clone()
    }
}
pub fn fixture(name: &str) -> Value {
    serde_json::from_str(match name {
        "session" => include_str!("../fixtures/session.json"),
        "message" => include_str!("../fixtures/message.json"),
        "set" => include_str!("../fixtures/scale-set.json"),
        "jit" => include_str!("../fixtures/jit.json"),
        "exists" => include_str!("../fixtures/agent-exists.json"),
        "busy" => include_str!("../fixtures/job-still-running.json"),
        _ => panic!("unknown fixture"),
    })
    .unwrap()
}
pub fn jwt(exp: u64) -> String {
    format!(
        "e30.{}.signature",
        URL_SAFE_NO_PAD.encode(json!({"exp":exp}).to_string())
    )
}
pub fn config(server: &MockServer, clock: Arc<FakeClock>) -> Config {
    let mut config = Config::new(
        "https://github.com/example-org/example-repo",
        Credentials::Pat(Secret::new("synthetic-pat")),
    )
    .unwrap();
    config.github_api_url = server.uri().parse().unwrap();
    config.clock = clock;
    config
}
pub async fn auth(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(
            "/repos/example-org/example-repo/actions/runners/registration-token",
        ))
        .and(header("Authorization", "Bearer synthetic-pat"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"token":"synthetic-registration"})),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/actions/runner-registration"))
        .and(header("Authorization", "RemoteAuth synthetic-registration"))
        .and(body_json(
            json!({"url":"https://github.com/example-org/example-repo","runner_event":"register"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"url":server.uri(),"token":jwt(NOW+3600)})),
        )
        .mount(server)
        .await;
}
pub async fn harness() -> (MockServer, ActionsClient, Arc<FakeClock>) {
    let server = MockServer::start().await;
    let clock = Arc::new(FakeClock::default());
    auth(&server).await;
    let client = ActionsClient::new(config(&server, clock.clone())).unwrap();
    (server, client, clock)
}
pub fn session(server: &MockServer) -> Value {
    let mut value = fixture("session");
    value["messageQueueUrl"] = json!(format!("{}/queue?partition=synthetic", server.uri()));
    value
}
pub async fn sessions(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(SESSIONS))
        .and(body_json(json!({"ownerName":"test-owner"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(session(server)))
        .mount(server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(SESSION))
        .respond_with(ResponseTemplate::new(204))
        .mount(server)
        .await;
}
pub async fn count(server: &MockServer, verb: &str, endpoint: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == verb && r.url.path() == endpoint)
        .count()
}
pub async fn versions(server: &MockServer) {
    for request in server.received_requests().await.unwrap() {
        assert!(
            request
                .url
                .query_pairs()
                .any(|(k, v)| k == "api-version" && v == "6.0-preview"),
            "{}",
            request.url
        );
    }
}
