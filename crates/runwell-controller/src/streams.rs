//! Full snapshots make event-stream reconnect independent of a retained cursor.
use crate::Fleet;
use runwell_transport::{Client, Report};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub(crate) fn subscribe(
    fleet: Arc<Fleet>,
    clients: Vec<Arc<Client>>,
    stop: CancellationToken,
) -> Vec<tokio::task::JoinHandle<()>> {
    let (send, mut receive) = tokio::sync::mpsc::channel::<Report>(64);
    let mut tasks = Vec::new();
    for client in clients {
        let send = send.clone();
        let stop = stop.clone();
        tasks.push(tokio::spawn(async move {
            let mut backoff = 1;
            loop {
                tokio::select! { _ = stop.cancelled() => break, _ = client.watch(send.clone()) => {} }
                tokio::select! { _ = stop.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(backoff)) => {} }
                backoff = (backoff * 2).min(30);
            }
        }));
    }
    drop(send);
    tasks.push(tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                report = receive.recv() => {
                    let Some(report) = report else { break; };
                    // Client::watch checked the report ID against its verified peer.
                    let id = report.node_id.clone();
                    if let Err(error) = fleet.accept_report(&id, report).await { tracing::warn!(%error, "node event snapshot rejected"); }
                }
            }
        }
    }));
    tasks
}
