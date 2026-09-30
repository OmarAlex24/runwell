#![allow(dead_code)]
#[path = "../../../runwell-scaleset/tests/common/mod.rs"]
pub mod protocol;
pub use protocol::{AGENTS, SESSION, SESSIONS, count, fixture};
use runwell_config::Config;
use runwell_node::{Controller, GithubGateway, fake::FakeBackend};
use runwell_store::Store;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
pub const ACQUIRE: &str = "/_apis/runtime/runnerscalesets/42/acquirejobs";
pub const JIT: &str = "/_apis/runtime/runnerscalesets/42/generatejitconfig";
pub struct Harness {
    pub server: MockServer,
    pub gateway: Arc<GithubGateway>,
    pub backend: Arc<FakeBackend>,
    pub controller: Controller,
    pub store: Store,
    pub config: Config,
    pub directory: tempfile::TempDir,
}
impl Harness {
    pub async fn new(max: u32) -> Self {
        let (server, client, _) = protocol::harness().await;
        let directory = tempfile::tempdir().unwrap();
        let mut config =
            Config::from_toml(include_str!("../../../../examples/runwell.toml")).unwrap();
        config.node.state_dir = directory.path().to_owned();
        config.controller.database = directory.path().join("state.sqlite");
        let standalone = config.standalone.as_mut().unwrap();
        standalone.templates_dir = directory.path().join("templates");
        standalone.runners_dir = directory.path().join("runners");
        standalone.max_jobs = max;
        standalone.reconcile_seconds = 1;
        standalone.drain_seconds = 2;
        let store = Store::open(config.controller.database.to_str().unwrap())
            .await
            .unwrap();
        let backend = Arc::new(FakeBackend::default());
        let gateway = Arc::new(GithubGateway::new(client));
        let classes = BTreeMap::from([(42, config.controller.classes[0].clone())]);
        let controller = Controller::new(
            &config,
            classes,
            store.clone(),
            backend.clone(),
            gateway.clone(),
        )
        .unwrap();
        let mut h = Self {
            server,
            gateway,
            backend,
            controller,
            store,
            config,
            directory,
        };
        h.controller.reconcile().await.unwrap();
        let mut session = protocol::session(&h.server);
        session["statistics"] = json!({});
        Mock::given(method("POST"))
            .and(path(SESSIONS))
            .respond_with(ResponseTemplate::new(200).set_body_json(session))
            .mount(&h.server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(SESSION))
            .respond_with(ResponseTemplate::new(204))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path(AGENTS))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"count":0,"value":[]})))
            .with_priority(10)
            .mount(&h.server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("{AGENTS}/1234")))
            .respond_with(ResponseTemplate::new(204))
            .with_priority(10)
            .mount(&h.server)
            .await;
        h.gateway.open_session(42, "test-owner", max).await.unwrap();
        let (_, initial) = h.gateway.next().await.unwrap();
        h.controller.handle(42, &initial, 30_000).await.unwrap();
        h
    }
    pub async fn normal_registration(&self) {
        Mock::given(method("POST"))
            .and(path(ACQUIRE))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"count": 1, "value": [101]})),
            )
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path(JIT))
            .respond_with(ResponseTemplate::new(200).set_body_json(jit()))
            .mount(&self.server)
            .await;
    }
    pub async fn message(&self, id: i64, events: Vec<Value>) -> runwell_scaleset::Message {
        self.message_stats(id, events, json!({})).await
    }
    pub async fn message_stats(
        &self,
        id: i64,
        events: Vec<Value>,
        stats: Value,
    ) -> runwell_scaleset::Message {
        Mock::given(method("GET")).and(path("/queue"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"messageId":id,"messageType":"RunnerScaleSetJobMessages","body":serde_json::to_string(&events).unwrap(),"statistics":stats})))
            .up_to_n_times(1).with_priority(1).mount(&self.server).await;
        Mock::given(method("DELETE"))
            .and(path(format!("/queue/{id}")))
            .respond_with(ResponseTemplate::new(204))
            .mount(&self.server)
            .await;
        self.gateway.next().await.unwrap().1
    }
    pub async fn deliver(&mut self, id: i64, events: Vec<Value>) {
        let message = self.message(id, events).await;
        self.controller
            .handle(42, &message, 30_000 + id.max(0) as u64 * 1000)
            .await
            .unwrap();
        self.gateway.acknowledge(42, id).await.unwrap();
        self.store.acked(42, id).await.unwrap();
    }
    pub async fn restart(&mut self) {
        let store = Store::open(self.config.controller.database.to_str().unwrap())
            .await
            .unwrap();
        self.controller = Controller::new(
            &self.config,
            BTreeMap::from([(42, self.config.controller.classes[0].clone())]),
            store.clone(),
            self.backend.clone(),
            self.gateway.clone(),
        )
        .unwrap();
        self.store = store;
        self.controller.reconcile().await.unwrap();
    }
}
pub fn available() -> Value {
    json!({"messageType":"JobAvailable","runnerRequestId":101,"jobId":"job-101","ownerName":"example-org","repositoryName":"example-repo","jobDisplayName":"unit","workflowRunId":20})
}
pub fn complete() -> Value {
    json!({"messageType":"JobCompleted","runnerRequestId":101,"runnerId":1234,"runnerName":"rw-node-1-j1","result":"succeeded"})
}
pub fn jit() -> Value {
    let mut value = fixture("jit");
    value["runner"]["name"] = json!("rw-node-1-j1");
    value
}
