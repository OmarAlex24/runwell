use crate::{Controller, Error, GithubGateway};
use runwell_scaleset::Message;
use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

/// Shutdown requests supplied by Unix signal wiring or deterministic tests.
#[derive(Debug, Clone, Copy)]
pub enum Drain {
    /// Stop admission and wait up to drain_seconds for running jobs.
    Graceful,
    /// Close sessions immediately, preserving live systemd units for re-adoption.
    Immediate,
}
/// Run the in-process controller loop. All session exits call close, including
/// errors. First signal drains; second signal/timeout preserves live units for
/// restart. Canceled I/O is recoverable from the journal, including ambiguous JIT.
/// Failed batches remain unacknowledged until successfully handled.
pub async fn run_loop(
    controller: &mut Controller,
    gateway: Arc<GithubGateway>,
    mut signals: mpsc::Receiver<Drain>,
) -> Result<(), Error> {
    let result = drive(controller, &gateway, &mut signals).await;
    let closed = gateway.close().await;
    result.and(closed)
}
enum Wake<T> {
    Work(T),
    Signal(Option<Drain>),
    Expired,
}
async fn interruptible<T>(
    work: impl Future<Output = T>,
    signals: &mut mpsc::Receiver<Drain>,
    open: bool,
    deadline: Option<Instant>,
) -> Wake<T> {
    let timeout = async {
        if let Some(deadline) = deadline {
            tokio::time::sleep_until(deadline.into()).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        biased;
        signal = signals.recv(), if open => Wake::Signal(signal),
        _ = timeout => Wake::Expired,
        result = work => Wake::Work(result),
    }
}
fn signal(
    controller: &mut Controller,
    request: Option<Drain>,
    open: &mut bool,
    deadline: &mut Option<Instant>,
) -> bool {
    match request {
        Some(Drain::Immediate) => true,
        Some(Drain::Graceful) if controller.is_draining() => true,
        Some(Drain::Graceful) => {
            controller.drain();
            *deadline =
                Some(Instant::now() + Duration::from_secs(controller.settings.drain_seconds));
            false
        }
        None => {
            *open = false;
            false
        }
    }
}
async fn drive(
    controller: &mut Controller,
    gateway: &GithubGateway,
    signals: &mut mpsc::Receiver<Drain>,
) -> Result<(), Error> {
    let start = Instant::now();
    let mut timer =
        tokio::time::interval(Duration::from_secs(controller.settings.reconcile_seconds));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut deadline = None;
    let mut pending: Option<(i64, Message)> = None;
    let mut signals_open = true;
    loop {
        if controller.is_draining() && controller.is_idle().await? {
            return Ok(());
        }
        let wait = async {
            tokio::select! {
                batch = gateway.next(), if pending.is_none() => batch.map(Some),
                _ = timer.tick() => Ok(None),
            }
        };
        match interruptible(wait, signals, signals_open, deadline).await {
            Wake::Expired => return Ok(()),
            Wake::Signal(request) => {
                if signal(controller, request, &mut signals_open, &mut deadline) {
                    return Ok(());
                }
                for set in controller.classes.keys() {
                    gateway.capacity(*set, 0).await;
                }
                continue;
            }
            Wake::Work(batch) => {
                if let Some(batch) = batch? {
                    pending = Some(batch);
                }
            }
        }
        let work = async {
            if let Some((set, message)) = &pending {
                controller.handle(*set, message, elapsed(start)).await?;
                if let Some(id) = message.message_id {
                    gateway.acknowledge(*set, id).await?;
                    controller.store.acked(*set, id).await?;
                }
            } else {
                controller.tick(elapsed(start)).await?;
            }
            Ok::<_, Error>(())
        };
        match interruptible(work, signals, signals_open, deadline).await {
            Wake::Expired => return Ok(()),
            Wake::Signal(request) => {
                if signal(controller, request, &mut signals_open, &mut deadline) {
                    return Ok(());
                }
                for set in controller.classes.keys() {
                    gateway.capacity(*set, 0).await;
                }
                continue;
            }
            Wake::Work(Ok(())) => pending = None,
            Wake::Work(Err(Error::SessionReset)) => {
                if let Some((set, message)) = &pending {
                    let current = if let Some(id) = message.message_id {
                        gateway.batch_pending(*set, id).await
                    } else {
                        false
                    };
                    if !current {
                        pending = None;
                    }
                }
            }
            Wake::Work(Err(error)) => {
                tracing::warn!(%error,"durable work retained for retry; batch remains unacknowledged")
            }
        }
        match controller.capacities(elapsed(start)).await {
            Ok(capacities) => {
                for (set, capacity) in capacities {
                    gateway.capacity(set, capacity).await;
                }
            }
            Err(error) => {
                for set in controller.classes.keys() {
                    gateway.capacity(*set, 0).await;
                }
                tracing::warn!(%error,"admission stopped until pressure can be read");
            }
        }
    }
}
fn elapsed(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}
