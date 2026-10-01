#![cfg(unix)]
mod support;
use hyper::{Response, StatusCode};
use runwell_dockerproxy::ProxyManager;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use support::*;
#[tokio::test]
async fn manager_detects_once_recovers_socket_and_cleans_only_exact_labels_in_order() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = calls.clone();
    let fixture = Fixture::new(move |request| {
        let recorded = recorded.clone();
        async move {
            let uri = request.uri().to_string();
            let path = request.uri().path();
            recorded.lock().unwrap().push(format!("{} {uri}", request.method()));
            let owned = json!({"io.runwell.job":"42","io.runwell.node":"node-a"});
            let wrong_node = json!({"io.runwell.job":"42","io.runwell.node":"node-b"});
            let other_job = json!({"io.runwell.job":"43","io.runwell.node":"node-a"});
            let value = if path.ends_with("/version") { json!({"ApiVersion":"1.47"}) }
                else if path.ends_with("/info") { json!({"CgroupDriver":"systemd"}) }
                else if path.ends_with("/containers/json") {
                    json!([{"Id":"owned","Labels":owned},{"Id":"control","Labels":{}},{"Id":"foreign","Labels":wrong_node},{"Id":"other-job","Labels":other_job}])
                } else if path.ends_with("/networks") {
                    json!([{"Id":"network","Labels":owned},{"Id":"unlabeled-net","Labels":{}}])
                } else if path.ends_with("/volumes") {
                    let volume = |name, labels| json!({"Name":name,"Driver":"local","Mountpoint":"/data","Scope":"local","Labels":labels,"Options":{}});
                    json!({"Volumes":[volume("volume",owned),volume("unlabeled-vol",json!({}))]})
                } else {
                    // Concurrent deletion and already-stopped resources are fine.
                    let status = if path.ends_with("/stop") { StatusCode::NOT_MODIFIED } else { StatusCode::NOT_FOUND };
                    return Response::builder().status(status).body(full(r#"{"message":"already gone"}"#)).unwrap();
                };
            Response::new(full(value.to_string()))
        }
    }).await;
    let manager = ProxyManager::new(fixture.settings.clone(), "node-a".into());
    manager.ensure(fixture.spec.clone()).await.unwrap();
    manager.ensure(fixture.spec.clone()).await.unwrap();
    assert_eq!(
        manager
            .inventory()
            .await
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![42, 43]
    );
    // Dropping a node closes its listener but deliberately leaves the stale inode.
    drop(manager);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let manager = ProxyManager::new(fixture.settings.clone(), "node-a".into());
    manager.ensure(fixture.spec.clone()).await.unwrap();
    manager.cleanup(42).await.unwrap();
    manager.cleanup(42).await.unwrap();
    assert!(!fixture.socket().exists());
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.iter().filter(|c| c.ends_with("/info")).count(),
        2,
        "once per manager including restart"
    );
    let mutations: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("POST") || c.starts_with("DELETE"))
        .collect();
    assert_eq!(mutations.len(), 16);
    for chunk in mutations.chunks(4) {
        assert!(
            chunk[0].starts_with("POST /containers/owned/stop?t=10"),
            "{chunk:?}"
        );
        assert!(chunk[1].starts_with("DELETE /containers/owned?"));
        assert!(chunk[1].contains("v=true"));
        assert_eq!(chunk[2].as_str(), "DELETE /networks/network");
        assert_eq!(chunk[3].as_str(), "DELETE /volumes/volume?force=true");
    }
    for call in calls.iter().filter(|c| c.contains("filters=")) {
        let query = call.split_once('?').unwrap().1;
        let pairs: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(query.as_bytes()).collect();
        let filters: Value = serde_json::from_str(&pairs["filters"]).unwrap();
        assert!(
            filters["label"]
                .as_array()
                .unwrap()
                .contains(&json!("io.runwell.node=node-a"))
        );
    }
}
#[tokio::test]
async fn shutdown_cancels_an_open_stream_and_removes_listener() {
    let mut fixture = Fixture::new(|_| async {
        let stream = futures_util::stream::pending::<
            Result<hyper::body::Frame<bytes::Bytes>, std::convert::Infallible>,
        >();
        use http_body_util::BodyExt;
        Response::new(
            http_body_util::StreamBody::new(stream)
                .map_err(BoxError::from)
                .boxed_unsync(),
        )
    })
    .await;
    fixture.start().await;
    let response = fixture.send("GET", "/events", full("")).await;
    fixture.stop().await;
    use http_body_util::BodyExt;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            response.into_body().collect()
        )
        .await
        .unwrap()
        .is_err()
    );
}

#[tokio::test]
async fn shutdown_waits_for_an_accepted_create_before_cleanup_can_list() {
    use http_body_util::BodyExt;
    use std::time::Duration;
    let received = Arc::new(tokio::sync::Semaphore::new(0));
    let finish = Arc::new(tokio::sync::Semaphore::new(0));
    let (up_received, up_finish) = (received.clone(), finish.clone());
    let mut fixture = Fixture::new(move |request| {
        let (received, finish) = (up_received.clone(), up_finish.clone());
        async move {
            request.into_body().collect().await.unwrap();
            received.add_permits(1);
            finish.acquire().await.unwrap().forget();
            Response::new(full("{}"))
        }
    })
    .await;
    fixture.start().await;
    use tokio::io::AsyncWriteExt;
    let mut client = tokio::net::UnixStream::connect(fixture.socket())
        .await
        .unwrap();
    client
        .write_all(
            b"POST /containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: 2\r\n\r\n{}",
        )
        .await
        .unwrap();
    received.acquire().await.unwrap().forget();
    drop(client); // A disconnected client must not release the mutation barrier.
    let mut shutdown = tokio::spawn(fixture.proxy.take().unwrap().shutdown());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut shutdown)
            .await
            .is_err()
    );
    finish.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!fixture.socket().exists());
}
