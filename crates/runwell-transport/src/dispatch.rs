use crate::{Error, Handler, Identity, Request, Response};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, watch};

type ResultChannel = watch::Receiver<Option<Result<Response, Error>>>;
type CallKey = (Identity, [u8; 32]);

/// One budget across connections, streams and surviving detached mutations.
pub(crate) struct Dispatcher {
    handler: Arc<dyn Handler>,
    permits: Arc<Semaphore>,
    pending: Mutex<BTreeMap<CallKey, ResultChannel>>,
}
impl Dispatcher {
    pub fn new(handler: Arc<dyn Handler>, limit: usize) -> Arc<Self> {
        Arc::new(Self {
            handler,
            permits: Arc::new(Semaphore::new(limit)),
            pending: Mutex::new(BTreeMap::new()),
        })
    }
    /// Called before polling or allocating the request body. No unbounded wait queue.
    pub fn reserve(&self) -> Result<OwnedSemaphorePermit, Error> {
        self.permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)
    }
    pub async fn report(&self, peer: Identity) -> Result<Response, Error> {
        self.handler.handle(peer, Request::Report).await
    }
    pub async fn call(
        self: &Arc<Self>,
        peer: Identity,
        request: Request,
        permit: OwnedSemaphorePermit,
    ) -> Result<Response, Error> {
        // Include the authenticated peer and entire canonical request. Conflicting
        // payloads never borrow another request's successful result. Store only
        // the digest, never a second copy of a JIT credential.
        let key = if request.key().is_some() {
            Some((
                peer.clone(),
                Sha256::digest(serde_json::to_vec(&request).map_err(|_| Error::Protocol)?).into(),
            ))
        } else {
            None
        };
        let mut pending = self.pending.lock().await;
        let mut result = if let Some(existing) = key.as_ref().and_then(|k| pending.get(k)) {
            drop(request);
            drop(permit);
            existing.clone()
        } else {
            let (send, receive) = watch::channel(None);
            if let Some(key) = &key {
                pending.insert(key.clone(), receive.clone());
            }
            let dispatcher = self.clone();
            tokio::spawn(async move {
                use futures_util::FutureExt;
                // The permit follows the handler through disconnects, mutex waits
                // and completion. A panic must also release its coalescing entry.
                let _permit = permit;
                let result = std::panic::AssertUnwindSafe(dispatcher.handler.handle(peer, request))
                    .catch_unwind()
                    .await
                    .unwrap_or(Err(Error::Backend));
                let _ = send.send(Some(result));
                if let Some(key) = key {
                    dispatcher.pending.lock().await.remove(&key);
                }
            });
            receive
        };
        drop(pending);
        loop {
            if let Some(response) = result.borrow_and_update().clone() {
                return response;
            }
            result.changed().await.map_err(|_| Error::Backend)?;
        }
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
