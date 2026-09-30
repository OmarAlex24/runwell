//! Backpressured stream of batches. A delivery must be acknowledged explicitly
//! before the next poll. Acquisition and caller-side durable work precede ack.
use crate::{Error, events::Message, retry::RetryConfig, session::Session};
use futures_core::Stream;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

type PollFuture = Pin<Box<dyn Future<Output = Result<Message, Error>> + Send>>;

/// Async stream of decoded batches, with startup statistics as its first item.
/// Unknown envelopes yield an empty batch with a real ID so explicit ack can
/// advance the queue. Calling `next` again before ack immediately returns
/// [`Error::AckRequired`] without polling HTTP or discarding the pending delivery.
/// Empty polls returning in less than five seconds use jittered exponential
/// backoff (at least one second, capped at 30); a normal long poll resets it.
///
/// On processing failure, call `close` without acknowledging; the service can
/// redeliver to the next session. Process events idempotently across restarts.
#[must_use = "close the listener explicitly on graceful shutdown"]
pub struct Listener {
    session: Session,
    capacity: Arc<AtomicU32>,
    future: Option<PollFuture>,
    pending: Option<(i64, u64)>,
    initial: bool,
    last: i64,
    generation: u64,
}
impl Listener {
    /// Own an existing session and advertise the current maximum capacity.
    pub fn new(session: Session, max_capacity: u32) -> Self {
        Self {
            session,
            capacity: Arc::new(AtomicU32::new(max_capacity)),
            future: None,
            pending: None,
            initial: true,
            last: 0,
            generation: 0,
        }
    }
    /// Change capacity for the next HTTP poll, including polls following a 202.
    pub fn set_max_capacity(&self, max: u32) {
        self.capacity.store(max, Ordering::Release);
    }
    /// Access the session for explicit acquisition or renewal.
    pub fn session(&self) -> &Session {
        &self.session
    }
    /// Acquire a chosen subset of the delivered JobAvailable IDs. This does not ack.
    pub async fn acquire_jobs(&self, ids: &[i64]) -> Result<Vec<i64>, Error> {
        self.session.acquire_jobs(ids).await
    }
    /// Acknowledge only the current batch, after acquisition and all handling
    /// succeeded. Updates `lastMessageId` only after the DELETE succeeds.
    pub async fn ack(&mut self, message_id: i64) -> Result<(), Error> {
        let Some((pending, generation)) = self.pending else {
            return Err(Error::InvalidAck);
        };
        if pending != message_id {
            return Err(Error::InvalidAck);
        }
        match self.session.core.ack(message_id, generation).await {
            Ok(()) => {
                self.pending = None;
                self.last = message_id;
                Ok(())
            }
            Err(Error::SessionRecreated) => {
                self.pending = None;
                self.last = 0;
                self.initial = true;
                Err(Error::SessionRecreated)
            }
            Err(error) => Err(error),
        }
    }
    /// Cancel an outstanding poll and delete the session, even with an unacked batch.
    pub async fn close(mut self) -> Result<(), Error> {
        self.future = None;
        self.session.close().await
    }
}
impl Stream for Listener {
    type Item = Result<Message, Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.pending.is_some() {
            return Poll::Ready(Some(Err(Error::AckRequired)));
        }
        if this.future.is_none() {
            let core = this.session.core.clone();
            let capacity = this.capacity.clone();
            let initial = this.initial;
            let last = this.last;
            let generation = this.generation;
            this.future = Some(Box::pin(async move {
                if initial {
                    return Ok(core.initial().await);
                }
                let clock = core.clock();
                let backoff = RetryConfig::default();
                let mut rapid_empty_polls = 0;
                loop {
                    let started = clock.now();
                    if let Some(message) = core
                        .poll(last, capacity.load(Ordering::Acquire), generation)
                        .await?
                    {
                        return Ok(message);
                    }
                    let elapsed = clock.now().duration_since(started).unwrap_or_default();
                    if elapsed < Duration::from_secs(5) {
                        // A fast 202 can signal an unhealthy service or proxy. Keep
                        // throttling even if transport retries were configured off.
                        let delay = backoff.delay(rapid_empty_polls).max(Duration::from_secs(1));
                        clock.sleep(delay).await;
                        rapid_empty_polls = rapid_empty_polls.saturating_add(1);
                    } else {
                        rapid_empty_polls = 0;
                    }
                }
            }));
        }
        let Some(future) = &mut this.future else {
            return Poll::Pending;
        };
        match future.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                this.future = None;
                if let Ok(message) = &result {
                    this.initial = false;
                    this.generation = message.generation;
                    if let Some(id) = message.message_id {
                        this.pending = Some((id, message.generation));
                    } else {
                        this.last = 0;
                    }
                }
                Poll::Ready(Some(result))
            }
        }
    }
}
