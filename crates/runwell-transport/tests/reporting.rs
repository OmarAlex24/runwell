use runwell_node::Drain;
use runwell_transport::{
    DrainJournal, Error, JobStatus, Key, Report, Request, Response, Rpc, RpcFuture,
    report_until_drained,
};
use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, mpsc};

struct Reporter {
    store: Journal,
    failure: bool,
    stall: bool,
    idle: bool,
    entered: Notify,
}
impl Rpc for Reporter {
    fn call(&self, request: Request) -> RpcFuture<'_> {
        Box::pin(async move {
            assert!(matches!(request, Request::Report));
            self.entered.notify_one();
            if self.stall {
                return pending().await;
            }
            if self.failure {
                return Err(Error::Backend);
            }
            Ok(Response::Report(Report {
                node_id: "node-1".into(),
                sequence: 1,
                observed_at_ms: 0,
                draining: self.store.0.load(Ordering::SeqCst),
                cpu_slots: 1,
                memory_bytes: 1024,
                reserved_cpu: 1,
                reserved_memory: 1024,
                free_slots: 0,
                pressure: [0.0; 4],
                template_version: "2.337.0".into(),
                jobs: if self.idle {
                    vec![]
                } else {
                    vec![JobStatus {
                        key: Key {
                            job_id: 1,
                            attempt: 1,
                        },
                        phase: runwell_transport::STARTED,
                        process: runwell_node::ProcessState::Running,
                        started_at_ms: Some(0),
                        heartbeat_at_ms: None,
                    }]
                },
                inventory: vec![],
            }))
        })
    }
}
#[derive(Default)]
struct Partition {
    entered: Notify,
    cancelled: AtomicUsize,
}
struct InFlight<'a>(&'a AtomicUsize);
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl Rpc for Partition {
    fn call(&self, request: Request) -> RpcFuture<'_> {
        Box::pin(async move {
            assert!(matches!(request, Request::Register(_)));
            let _guard = InFlight(&self.cancelled);
            self.entered.notify_one();
            pending().await
        })
    }
}
#[derive(Default)]
struct Journal(AtomicBool);
impl DrainJournal for Journal {
    fn stop_admissions(&self) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>> {
        Box::pin(async {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}
fn setup(failure: bool, stall: bool, idle: bool) -> (Arc<Reporter>, Arc<Partition>) {
    (
        Arc::new(Reporter {
            store: Journal::default(),
            failure,
            stall,
            idle,
            entered: Notify::new(),
        }),
        Arc::new(Partition::default()),
    )
}

fn run(
    agent: Arc<Reporter>,
    controller: Arc<Partition>,
    signals: mpsc::Receiver<Drain>,
) -> tokio::task::JoinHandle<Result<(), Error>> {
    tokio::spawn(async move {
        report_until_drained(
            agent.as_ref(),
            controller.as_ref(),
            &agent.store,
            signals,
            Duration::from_secs(1),
            Duration::from_secs(10),
        )
        .await
    })
}
#[tokio::test(start_paused = true)]
async fn inventory_failures_cannot_postpone_drain_deadline() {
    let (agent, controller) = setup(true, false, false);
    let (send, signals) = mpsc::channel(2);
    let task = run(agent.clone(), controller, signals);
    agent.entered.notified().await;
    send.send(Drain::Graceful).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(agent.store.0.load(Ordering::SeqCst));
}
#[tokio::test(start_paused = true)]
async fn stalled_inventory_is_interruptible_by_signal_and_deadline() {
    let (agent, controller) = setup(false, true, false);
    let (send, signals) = mpsc::channel(2);
    let task = run(agent.clone(), controller, signals);
    agent.entered.notified().await;
    send.send(Drain::Graceful).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(agent.store.0.load(Ordering::SeqCst));
}
#[tokio::test(start_paused = true)]
async fn stalled_registration_cannot_delay_second_signal() {
    let (agent, controller) = setup(false, false, false);
    let (send, signals) = mpsc::channel(2);
    let task = run(agent.clone(), controller.clone(), signals);
    controller.entered.notified().await;
    send.send(Drain::Graceful).await.unwrap();
    send.send(Drain::Immediate).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(agent.store.0.load(Ordering::SeqCst));
    assert_eq!(controller.cancelled.load(Ordering::SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn stalled_registration_cannot_delay_deadline_or_idle_drain() {
    for idle in [false, true] {
        let (agent, controller) = setup(false, false, idle);
        let (send, signals) = mpsc::channel(2);
        let task = run(agent.clone(), controller.clone(), signals);
        controller.entered.notified().await;
        send.send(Drain::Graceful).await.unwrap();
        let limit = Duration::from_secs(if idle { 5 } else { 15 });
        tokio::time::timeout(limit, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(controller.cancelled.load(Ordering::SeqCst), 1);
    }
}
