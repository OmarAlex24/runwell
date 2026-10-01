#![cfg(unix)]
mod support;
use hyper::{Response, StatusCode};
use runwell_dockerproxy::{Error, ProxyManager};
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::*;
use tokio::sync::Semaphore;

fn empty_response(path: &str) -> Response<Body> {
    let value = if path.ends_with("/version") {
        json!({"ApiVersion":"1.47"})
    } else if path.ends_with("/info") {
        json!({"CgroupDriver":"systemd"})
    } else if path.ends_with("/volumes") {
        json!({"Volumes":[]})
    } else {
        json!([])
    };
    Response::new(full(value.to_string()))
}

#[tokio::test]
async fn cleanup_stops_concurrently_without_blocking_other_jobs() {
    let started = Arc::new(Semaphore::new(0));
    let finish = Arc::new(Semaphore::new(0));
    let deleted = Arc::new(Mutex::new(Vec::new()));
    let (up_started, up_finish, up_deleted) = (started.clone(), finish.clone(), deleted.clone());
    let fixture = Fixture::new(move |request| {
        let (started, finish, deleted) =
            (up_started.clone(), up_finish.clone(), up_deleted.clone());
        async move {
            let path = request.uri().path();
            if path.ends_with("/containers/json") && deleted.lock().unwrap().is_empty() {
                return Response::new(full(
                    json!(["one","two","three"].map(|id|
                    json!({"Id":id,"Labels":{"io.runwell.job":"42","io.runwell.node":"node-a"}})))
                    .to_string(),
                ));
            }
            if path.ends_with("/stop") {
                assert_eq!(request.uri().query(), Some("t=10"));
                started.add_permits(1);
                finish.acquire().await.unwrap().forget();
                return Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(full(""))
                    .unwrap();
            }
            if request.method() == "DELETE" {
                deleted.lock().unwrap().push(path.to_owned());
                assert!(request.uri().query().unwrap().contains("v=true"));
                return Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(full(""))
                    .unwrap();
            }
            empty_response(path)
        }
    })
    .await;
    let manager = Arc::new(ProxyManager::new(fixture.settings.clone(), "node-a".into()));
    manager.ensure(fixture.spec.clone()).await.unwrap();
    let cleanup = {
        let manager = manager.clone();
        tokio::spawn(async move { manager.cleanup(42).await })
    };
    tokio::time::timeout(Duration::from_secs(2), started.acquire_many(3))
        .await
        .unwrap()
        .unwrap()
        .forget();
    let mut other = fixture.spec.clone();
    other.job_id = 43;
    other.cgroup_parent = "ci-rw-j43.slice".into();
    tokio::time::timeout(Duration::from_secs(2), manager.ensure(other))
        .await
        .unwrap()
        .unwrap();
    finish.add_permits(3);
    cleanup.await.unwrap().unwrap();
    assert_eq!(deleted.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn second_cleanup_pass_removes_objects_committed_after_first_inventory() {
    let lists = Arc::new(AtomicUsize::new(0));
    let deleted = Arc::new(Mutex::new(Vec::new()));
    let (up_lists, up_deleted) = (lists.clone(), deleted.clone());
    let fixture = Fixture::new(move |request| {
        let (lists, deleted) = (up_lists.clone(), up_deleted.clone());
        async move {
            let path = request.uri().path();
            if path.ends_with("/containers/json") && lists.fetch_add(1, Ordering::SeqCst) > 0 {
                return Response::new(full(json!([{"Id":"late","Labels":{"io.runwell.job":"42","io.runwell.node":"node-a"}}]).to_string()));
            }
            if request.method() == "DELETE" {
                deleted.lock().unwrap().push(path.to_owned());
                return Response::builder().status(StatusCode::NO_CONTENT).body(full("")).unwrap();
            }
            if path.ends_with("/stop") {
                return Response::builder().status(StatusCode::NO_CONTENT).body(full("")).unwrap();
            }
            empty_response(path)
        }
    }).await;
    let manager = ProxyManager::new(fixture.settings.clone(), "node-a".into());
    manager.ensure(fixture.spec.clone()).await.unwrap();
    manager.cleanup(42).await.unwrap();
    assert_eq!(lists.load(Ordering::SeqCst), 2);
    assert_eq!(*deleted.lock().unwrap(), ["/containers/late"]);
    assert!(!fixture.socket().exists());
}

#[tokio::test]
async fn unavailable_docker_allows_jobs_and_preserves_cleanup_identities() {
    let mut fixture =
        Fixture::new(|request| async move { empty_response(request.uri().path()) }).await;
    let mut settings = fixture.settings.clone();
    settings.upstream_socket = fixture.dir.path().join("absent.sock");
    let offline = ProxyManager::new(settings, "node-a".into());
    assert!(
        offline
            .prepare(fixture.spec.clone())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(offline.inventory().await.unwrap().is_empty());
    offline.cleanup(42).await.unwrap();
    offline.cleanup(42).await.unwrap();
    assert!(!fixture.socket().exists());

    let manager = ProxyManager::new(fixture.settings.clone(), "node-a".into());
    assert_eq!(
        manager.prepare(fixture.spec.clone()).await.unwrap().len(),
        2
    );
    // Simulate a daemon disappearing after its driver was cached.
    fixture.stop_upstream().await;
    let mut other = fixture.spec.clone();
    other.job_id = 43;
    other.cgroup_parent = "ci-rw-j43.slice".into();
    assert!(manager.prepare(other).await.unwrap().is_empty());
    manager.cleanup(43).await.unwrap();
    manager.cleanup(43).await.unwrap();
    assert!(matches!(manager.cleanup(42).await, Err(Error::Upstream)));
    assert!(
        !fixture.socket().exists(),
        "listener stops despite Docker outage"
    );
    assert!(
        fixture.socket().parent().unwrap().exists(),
        "retain identity for retry"
    );
    assert_eq!(
        manager
            .inventory()
            .await
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        [42]
    );
}

#[tokio::test]
async fn label_only_orphan_removal_errors_are_not_hidden_as_docker_unavailable() {
    let removals = Arc::new(AtomicUsize::new(0));
    let recorded = removals.clone();
    let fixture = Fixture::new(move |request| {
        let removals = recorded.clone();
        async move {
            let path = request.uri().path();
            if path.ends_with("/containers/json") {
                return Response::new(full(
                    json!([{"Id":"orphan","Labels":{
                    "io.runwell.job":"42","io.runwell.node":"node-a"}}])
                    .to_string(),
                ));
            }
            if request.method() == "DELETE" {
                removals.fetch_add(1, Ordering::SeqCst);
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(full(r#"{"message":"removal failed"}"#))
                    .unwrap();
            }
            empty_response(path)
        }
    })
    .await;
    let manager = ProxyManager::new(fixture.settings.clone(), "node-a".into());
    assert!(matches!(manager.cleanup(42).await, Err(Error::Upstream)));
    assert_eq!(
        removals.load(Ordering::SeqCst),
        2,
        "second pass runs even after failure"
    );
}
