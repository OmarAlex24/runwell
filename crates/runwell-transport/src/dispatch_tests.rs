use super::*;
use crate::{Key, RpcFuture};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

struct Blocked {
    calls: AtomicUsize,
    entered: Notify,
    release: Semaphore,
}
impl Handler for Blocked {
    fn handle(&self, _: Identity, _: Request) -> RpcFuture<'_> {
        Box::pin(async {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
            Ok(Response::Ok)
        })
    }
}
fn request(job: i64) -> Request {
    Request::Stop(Key {
        job_id: job,
        attempt: 1,
    })
}
fn caller(
    dispatcher: &Arc<Dispatcher>,
    job: i64,
) -> tokio::task::JoinHandle<Result<Response, Error>> {
    let permit = dispatcher.reserve().unwrap();
    let dispatcher = dispatcher.clone();
    tokio::spawn(async move {
        dispatcher
            .call(Identity::controller("controller-1"), request(job), permit)
            .await
    })
}
#[tokio::test]
async fn disconnected_handlers_stay_bounded_and_duplicate_lifecycle_calls_coalesce() {
    let handler = Arc::new(Blocked {
        calls: AtomicUsize::new(0),
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let dispatcher = Dispatcher::new(handler.clone(), 2);
    let first = caller(&dispatcher, 1);
    handler.entered.notified().await;
    first.abort();
    let _ = first.await;
    assert_eq!(
        dispatcher.permits.available_permits(),
        1,
        "disconnect cannot free a live handler's permit"
    );
    for _ in 0..16 {
        let duplicate = caller(&dispatcher, 1);
        // The duplicate drops its temporary body permit when it joins the first.
        while dispatcher.permits.available_permits() == 0 {
            tokio::task::yield_now().await;
        }
        duplicate.abort();
        let _ = duplicate.await;
    }
    assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
    assert_eq!(dispatcher.pending.lock().await.len(), 1);
    let second = caller(&dispatcher, 2);
    handler.entered.notified().await;
    second.abort();
    let _ = second.await;
    assert!(matches!(dispatcher.reserve(), Err(Error::Busy)));
    assert_eq!(handler.calls.load(Ordering::SeqCst), 2);
    handler.release.add_permits(2);
    while dispatcher.permits.available_permits() < 2 {
        tokio::task::yield_now().await;
    }
    assert!(dispatcher.pending.lock().await.is_empty());
}
#[tokio::test]
async fn conflicting_payload_does_not_share_a_successful_response() {
    let handler = Arc::new(Blocked {
        calls: AtomicUsize::new(0),
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let dispatcher = Dispatcher::new(handler.clone(), 2);
    let key = Key {
        job_id: 1,
        attempt: 1,
    };
    let mut tasks = Vec::new();
    for agent_id in [1, 2] {
        let dispatcher = dispatcher.clone();
        let permit = dispatcher.reserve().unwrap();
        tasks.push(tokio::spawn(async move {
            dispatcher
                .call(
                    Identity::controller("controller-1"),
                    Request::Start {
                        key,
                        agent_id,
                        jit: "test".into(),
                    },
                    permit,
                )
                .await
        }));
        handler.entered.notified().await;
    }
    assert_eq!(handler.calls.load(Ordering::SeqCst), 2);
    handler.release.add_permits(2);
    for task in tasks {
        assert!(task.await.unwrap().is_ok());
    }
}
