#![allow(dead_code)]
use runwell_controller::{Controller, Fleet, Hooks};
use runwell_node::{NodeFuture, fake::FakeBackend};
use runwell_store::{FailureEvent, Store};
use runwell_transport::{Agent, Clock, Request, Rpc, RpcFuture};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex, RwLock};

pub struct FakeClock(pub AtomicI64);
impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
impl FakeClock {
    pub fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}
#[derive(Default)]
pub struct Hook {
    pub events: Mutex<Vec<(i64, i64, String)>>,
}
impl Hooks for Hook {
    fn failure<'a>(&'a self, event: &'a FailureEvent) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            self.events
                .lock()
                .await
                .push((event.job_id, event.attempt, event.reason.clone()));
            Ok(())
        })
    }
}
pub struct Link {
    pub agent: RwLock<Arc<Agent>>,
    pub cut: AtomicBool,
    pub lose_start: AtomicBool,
    pub lose_admit: AtomicBool,
    pub prepare_ms: AtomicI64,
    pub clock: Arc<FakeClock>,
}
impl Rpc for Link {
    fn call(&self, request: Request) -> RpcFuture<'_> {
        Box::pin(async move {
            if self.cut.load(Ordering::SeqCst) {
                return Err(runwell_transport::Error::Timeout);
            }
            if matches!(request, Request::Prepare { .. }) {
                self.clock
                    .advance(self.prepare_ms.swap(0, Ordering::SeqCst));
            }
            let admit = matches!(request, Request::Admit { .. });
            let start = matches!(request, Request::Start { .. });
            let result = self.agent.read().await.call(request).await;
            if start && self.lose_start.swap(false, Ordering::SeqCst) {
                return Err(runwell_transport::Error::Timeout);
            }
            if admit && self.lose_admit.load(Ordering::SeqCst) {
                return Err(runwell_transport::Error::Timeout);
            }
            result
        })
    }
}
mod api;
pub use api::Api;
pub struct Harness {
    pub dir: tempfile::TempDir,
    pub config: runwell_config::Config,
    pub node_configs: Vec<runwell_config::Config>,
    pub stores: Vec<Store>,
    pub hosts: Vec<Arc<FakeBackend>>,
    pub links: Vec<Arc<Link>>,
    pub store: Store,
    pub api: Arc<Api>,
    pub hook: Arc<Hook>,
    pub clock: Arc<FakeClock>,
    pub fleet: Arc<Fleet>,
    pub controller: Controller,
    pub withhold_heartbeat: bool,
}
impl Harness {
    pub async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config =
            runwell_config::Config::from_toml(include_str!("../../../../examples/runwell.toml"))
                .unwrap();
        config.node.state_dir = dir.path().into();
        config.controller.database = dir.path().join("controller.sqlite");
        config.standalone.as_mut().unwrap().templates_dir = dir.path().join("templates");
        config.standalone.as_mut().unwrap().runners_dir = dir.path().join("runners");
        let clock = Arc::new(FakeClock(AtomicI64::new(1_800_000_000_000)));
        let store = Store::open(config.controller.database.to_str().unwrap())
            .await
            .unwrap();
        let mut stores = Vec::new();
        let mut node_configs = Vec::new();
        let mut hosts = Vec::new();
        let mut links = Vec::new();
        for index in 1..=2 {
            let mut cfg = config.clone();
            cfg.node.id = format!("node-{index}");
            cfg.standalone.as_mut().unwrap().max_jobs = 1;
            let db = dir.path().join(format!("node-{index}.sqlite"));
            let spool = Store::open(db.to_str().unwrap()).await.unwrap();
            let host = Arc::new(FakeBackend::default());
            let agent = Arc::new(
                Agent::open(cfg.clone(), spool.clone(), host.clone(), clock.clone())
                    .await
                    .unwrap(),
            );
            links.push(Arc::new(Link {
                agent: RwLock::new(agent),
                cut: AtomicBool::new(false),
                lose_start: AtomicBool::new(false),
                lose_admit: AtomicBool::new(false),
                prepare_ms: AtomicI64::new(0),
                clock: clock.clone(),
            }));
            hosts.push(host);
            stores.push(spool);
            node_configs.push(cfg);
        }
        let api = Arc::new(Api {
            store: store.clone(),
            nodes: stores.clone(),
            runners: Mutex::new(BTreeMap::new()),
            creates: AtomicUsize::new(0),
            busy: AtomicBool::new(false),
            lose_create: AtomicBool::new(false),
        });
        let hook = Arc::new(Hook::default());
        let fleet = fleet(&config, &store, &links, &api, &clock, &hook);
        let mut controller = Controller::new(
            &config,
            BTreeMap::from([(42, config.controller.classes[0].clone())]),
            store.clone(),
            fleet.clone(),
            api.clone(),
        )
        .unwrap();
        controller.reconcile().await.unwrap();
        Self {
            dir,
            config,
            node_configs,
            stores,
            hosts,
            links,
            store,
            api,
            hook,
            clock,
            fleet,
            controller,
            withhold_heartbeat: false,
        }
    }
    pub async fn queue(&self, request: i64) -> i64 {
        self.store
            .queue(runwell_store::NewJob {
                scale_set_id: 42,
                request_id: request,
                github_job_id: format!("job-{request}"),
                workflow_run_id: 1,
                repo: "example/project".into(),
                name: "test".into(),
                class: "runwell-small".into(),
                reserved_cpu: 1,
                reserved_memory: 2147483648,
            })
            .await
            .unwrap()
    }
    pub async fn tick(&mut self) {
        if !self.withhold_heartbeat {
            for host in &self.hosts {
                let mut state = host.state.lock().await;
                let ids: Vec<_> = state
                    .processes
                    .iter()
                    .filter(|(_, p)| **p == runwell_node::ProcessState::Running)
                    .map(|(id, _)| *id)
                    .collect();
                for id in ids {
                    state.heartbeats.insert(id, self.clock.now_ms());
                }
            }
        }
        self.controller
            .tick(self.clock.now_ms() as u64)
            .await
            .unwrap();
    }
    pub async fn restart_controller(&mut self) {
        let store = Store::open(self.config.controller.database.to_str().unwrap())
            .await
            .unwrap();
        self.fleet = fleet(
            &self.config,
            &store,
            &self.links,
            &self.api,
            &self.clock,
            &self.hook,
        );
        self.controller = Controller::new(
            &self.config,
            BTreeMap::from([(42, self.config.controller.classes[0].clone())]),
            store,
            self.fleet.clone(),
            self.api.clone(),
        )
        .unwrap();
        self.controller.reconcile().await.unwrap();
    }
    pub async fn restart_node(&self, index: usize) {
        let agent = Agent::open(
            self.node_configs[index].clone(),
            self.stores[index].clone(),
            self.hosts[index].clone(),
            self.clock.clone(),
        )
        .await
        .unwrap();
        *self.links[index].agent.write().await = Arc::new(agent);
    }
    pub async fn complete(&mut self, id: i64) {
        self.store
            .bind(
                id,
                runwell_store::Execution {
                    request_id: self.store.job(id).await.unwrap().metadata.request_id,
                    github_job_id: String::new(),
                    workflow_run_id: 1,
                    repo: "example/project".into(),
                    name: "test".into(),
                },
                Some("succeeded".into()),
            )
            .await
            .unwrap();
        for host in &self.hosts {
            let mut state = host.state.lock().await;
            if state.processes.contains_key(&(id as u64)) {
                state
                    .processes
                    .insert(id as u64, runwell_node::ProcessState::Exited(Some(0)));
            }
        }
        self.tick().await;
    }
    pub async fn no_leaks(&self) {
        let events = self.hook.events.lock().await;
        for job in self.store.jobs().await.unwrap() {
            assert!(
                job.state == runwell_store::State::Completed
                    || events.iter().filter(|(id, _, _)| *id == job.id).count() == 1,
                "job must complete or report one failure"
            );
        }
        assert!(self.api.runners.lock().await.is_empty());
        for host in &self.hosts {
            let state = host.state.lock().await;
            assert!(state.plans.is_empty());
            assert!(state.processes.is_empty());
            assert!(state.mounts.is_empty());
            assert!(state.containers.is_empty());
        }
        for store in &self.stores {
            assert!(store.active_leases().await.unwrap().is_empty());
        }
    }
}
fn fleet(
    config: &runwell_config::Config,
    store: &Store,
    links: &[Arc<Link>],
    api: &Arc<Api>,
    clock: &Arc<FakeClock>,
    hook: &Arc<Hook>,
) -> Arc<Fleet> {
    let peers = links
        .iter()
        .enumerate()
        .map(|(i, p)| (format!("node-{}", i + 1), p.clone() as Arc<dyn Rpc>))
        .collect();
    let policy = Arc::new(runwell_scheduler::Runwell {
        priority: runwell_scheduler::Priority::Fifo,
        aging_seconds: 300.0,
        admission: runwell_admission::ReservationAdmission::new(1.0, 1.0).unwrap(),
    });
    Arc::new(
        Fleet::new(
            config.clone(),
            store.clone(),
            peers,
            api.clone(),
            policy,
            clock.clone(),
            hook.clone(),
        )
        .unwrap(),
    )
}
