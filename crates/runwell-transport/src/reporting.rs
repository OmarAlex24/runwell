use crate::{Error, Request, Response, Rpc};
use runwell_node::Drain;
use runwell_store::Store;
use std::{
    future::{Future, pending},
    pin::Pin,
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    time::{Instant, MissedTickBehavior},
};

/// Durable drain control, separated from host operations so a stalled
/// preparation cannot block signals. Tests substitute an in-memory journal.
pub trait DrainJournal: Send + Sync {
    fn stop_admissions(&self) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>>;
}
impl DrainJournal for Store {
    fn stop_admissions(&self) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>> {
        Box::pin(async { self.drain_node().await.map_err(Error::from) })
    }
}

/// Interruptible node reporting and drain loop. Neither a failed local inventory
/// nor a partitioned controller can postpone signal handling or the drain deadline.
/// Exiting cancels only read/report RPC futures, never running host jobs.
pub async fn report_until_drained(
    agent: &dyn Rpc,
    controller: &dyn Rpc,
    store: &dyn DrainJournal,
    mut signals: mpsc::Receiver<Drain>,
    report_interval: Duration,
    drain_timeout: Duration,
) -> Result<(), Error> {
    let (reports, mut latest) = watch::channel(None);
    let (idle_send, mut idle_receive) = mpsc::channel(1);
    let reporting = async {
        let mut interval = tokio::time::interval(report_interval);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Ok(Response::Report(report)) = agent.call(Request::Report).await {
                let drained = report.draining && report.jobs.is_empty();
                reports.send_replace(Some(report));
                // Only the latest observation matters; never block inventory on
                // a slow registration or an undrained notifications channel.
                let _ = idle_send.try_send(drained);
            }
        }
    };
    let registering = async {
        loop {
            if latest.changed().await.is_err() {
                return;
            }
            let report = latest.borrow_and_update().clone();
            if let Some(report) = report {
                let _ = controller.call(Request::Register(report)).await;
            }
        }
    };
    tokio::pin!(reporting, registering);
    let mut deadline = None;
    loop {
        tokio::select! {
            biased;
            _ = async {
                if let Some(at) = deadline { tokio::time::sleep_until(at).await; }
                else { pending::<()>().await; }
            } => return Ok(()),
            signal = signals.recv() => {
                let immediate = matches!(signal, None | Some(Drain::Immediate)) || deadline.is_some();
                deadline = Some(Instant::now() + drain_timeout);
                store.stop_admissions().await?;
                if immediate { return Ok(()); }
            }
            Some(drained) = idle_receive.recv() => {
                if drained && deadline.is_some() { return Ok(()); }
            }
            _ = &mut reporting => return Err(Error::Io),
            _ = &mut registering => return Err(Error::Io),
        }
    }
}
