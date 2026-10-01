mod support;
use runwell_controller::{Controller, Fleet};
use runwell_transport::{Client, Identity, Request, Rpc, Tls, certs};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

fn tls(root: &std::path::Path) -> Tls {
    Tls::load(&runwell_config::TransportConfig {
        ca_file: root.join("ca.pem"),
        certificate_file: root.join("identity.pem"),
        private_key_file: root.join("key.pem"),
    })
    .unwrap()
}
#[tokio::test]
#[ignore = "real TLS sockets: run explicitly on localhost"]
async fn two_nodes_real_mtls_rotation_reconnect_and_event_stream() {
    let mut h = support::Harness::new().await;
    let ca = h.dir.path().join("ca");
    certs::create_ca(&ca, 30).unwrap();
    let controller_dir = h.dir.path().join("controller-tls");
    certs::issue(
        &ca,
        &controller_dir,
        &Identity::controller("controller-1"),
        7,
    )
    .unwrap();
    let controller_tls = tls(&controller_dir);
    let stop = CancellationToken::new();
    let mut tasks = Vec::new();
    let mut node_stops = Vec::new();
    let mut peers: BTreeMap<String, Arc<dyn Rpc>> = BTreeMap::new();
    let mut clients = Vec::new();
    let mut addresses = Vec::new();
    for i in 0..2 {
        let id = format!("node-{}", i + 1);
        let path = h.dir.path().join(&id);
        certs::issue(&ca, &path, &Identity::node(&id), 7).unwrap();
        let node_tls = tls(&path);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        addresses.push(address.clone());
        let client = Arc::new(
            Client::new(
                address,
                Identity::node(&id),
                &controller_tls,
                Duration::from_secs(5),
                2,
            )
            .unwrap(),
        );
        peers.insert(id, client.clone());
        clients.push(client);
        let handler = h.links[i].agent.read().await.clone();
        let shutdown = stop.child_token();
        node_stops.push(shutdown.clone());
        tasks.push(tokio::spawn(async move {
            runwell_transport::serve(
                listener,
                &node_tls,
                [Identity::controller("controller-1")].into(),
                handler,
                shutdown,
            )
            .await
        }));
    }
    let policy = Arc::new(runwell_scheduler::Runwell {
        priority: runwell_scheduler::Priority::Fifo,
        aging_seconds: 300.0,
        admission: runwell_admission::ReservationAdmission::new(1.0, 1.0).unwrap(),
    });
    let fleet = Arc::new(
        Fleet::new(
            h.config.clone(),
            h.store.clone(),
            peers,
            h.api.clone(),
            policy,
            h.clock.clone(),
            h.hook.clone(),
        )
        .unwrap(),
    );
    h.controller = Controller::new(
        &h.config,
        BTreeMap::from([(42, h.config.controller.classes[0].clone())]),
        h.store.clone(),
        fleet,
        h.api.clone(),
    )
    .unwrap();
    h.controller.reconcile().await.unwrap();
    let first = h.queue(101).await;
    let second = h.queue(102).await;
    h.tick().await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let watch = clients[0].clone();
    let watching = tokio::spawn(async move { watch.watch(tx).await });
    let report = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(!report.jobs.is_empty());
    watching.abort();
    // New certificate/key, same stable SAN: old/new accepted during overlap.
    let rotated = h.dir.path().join("controller-next");
    certs::issue(&ca, &rotated, &Identity::controller("controller-1"), 7).unwrap();
    let new_client = Client::new(
        addresses[0].clone(),
        Identity::node("node-1"),
        &tls(&rotated),
        Duration::from_secs(5),
        1,
    )
    .unwrap();
    assert!(new_client.call(Request::Report).await.is_ok());
    assert!(clients[0].call(Request::Report).await.is_ok());
    // Same CA with another identity or role grants no command authority.
    let unknown = h.dir.path().join("unknown");
    certs::issue(&ca, &unknown, &Identity::node("rogue"), 7).unwrap();
    let rogue = Client::new(
        addresses[0].clone(),
        Identity::node("node-1"),
        &tls(&unknown),
        Duration::from_secs(5),
        1,
    )
    .unwrap();
    assert!(rogue.call(Request::Drain).await.is_err());
    // Both sides reject a different CA, even with the authorized SAN spelling.
    let foreign_ca = h.dir.path().join("foreign-ca");
    certs::create_ca(&foreign_ca, 30).unwrap();
    let foreign = h.dir.path().join("foreign-controller");
    certs::issue(
        &foreign_ca,
        &foreign,
        &Identity::controller("controller-1"),
        7,
    )
    .unwrap();
    let untrusted = Client::new(
        addresses[0].clone(),
        Identity::node("node-1"),
        &tls(&foreign),
        Duration::from_secs(5),
        1,
    )
    .unwrap();
    assert!(untrusted.call(Request::Drain).await.is_err());

    // Drop the listener AND accepted connections, then reuse the original client.
    node_stops[0].cancel();
    tasks.remove(0).await.unwrap().unwrap();
    let listener = tokio::net::TcpListener::bind(&addresses[0]).await.unwrap();
    let node_tls = tls(&h.dir.path().join("node-1"));
    let handler = h.links[0].agent.read().await.clone();
    let shutdown = stop.child_token();
    tasks.push(tokio::spawn(async move {
        runwell_transport::serve(
            listener,
            &node_tls,
            [Identity::controller("controller-1")].into(),
            handler,
            shutdown,
        )
        .await
    }));
    assert!(clients[0].call(Request::Report).await.is_ok());
    h.complete(first).await;
    h.complete(second).await;
    h.no_leaks().await;
    stop.cancel();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
}
