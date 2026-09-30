use crate::{Error, NodeFuture};
use futures_util::{FutureExt, StreamExt, future::select_all};
use runwell_scaleset::{
    ActionsClient, JitSettings, Listener, Message, RemoveRunnerResult, RunnerReference,
};
use secrecy::SecretString;
use std::collections::BTreeMap;
use tokio::sync::Mutex;

/// Registration result; no Debug implementation can expose JIT credentials.
pub struct Registration {
    /// GitHub agent identity.
    pub agent_id: i64,
    /// Secret startup data, used once and never stored.
    pub jit: SecretString,
}
/// Controller-side GitHub operations, separate from the local-node interface.
pub trait RunnerApi: Send + Sync {
    /// Acquire only after durable host admission.
    fn acquire(&self, set: i64, request: i64) -> NodeFuture<'_, bool>;
    /// Create a runner using its previously journaled unique name.
    fn create<'a>(&'a self, set: i64, name: &'a str) -> NodeFuture<'a, Registration>;
    /// Recover an ambiguous creation outcome by durable name.
    fn lookup<'a>(&'a self, name: &'a str) -> NodeFuture<'a, Option<RunnerReference>>;
    /// Atomic DELETE-first busy check; false means keep and retry.
    fn delete(&self, agent: i64) -> NodeFuture<'_, bool>;
}
/// One scale-set listener per class, with concurrent long polls and explicit ack.
/// Poll cancellation retains each Listener's internal future, so timer/signal
/// handling cannot lose a delivered batch. Call close on all graceful paths.
pub struct GithubGateway {
    client: ActionsClient,
    listeners: Mutex<BTreeMap<i64, Delivery>>,
}
struct Delivery {
    listener: Listener,
    pending: Option<i64>,
    acked: Option<i64>,
}
impl GithubGateway {
    /// Construct without network activity; reconciliation can run before sessions.
    pub fn new(client: ActionsClient) -> Self {
        Self {
            client,
            listeners: Mutex::new(BTreeMap::new()),
        }
    }
    /// Open a single session for this set. Duplicate local opens are no-ops.
    pub async fn open_session(&self, set: i64, owner: &str, capacity: u32) -> Result<(), Error> {
        let mut listeners = self.listeners.lock().await;
        if listeners.contains_key(&set) {
            return Ok(());
        }
        let session = self.client.open_session(set, owner).await?;
        listeners.insert(
            set,
            Delivery {
                listener: Listener::new(session, capacity),
                pending: None,
                acked: None,
            },
        );
        Ok(())
    }
    /// Receive the next batch from any configured class, including startup stats.
    pub async fn next(&self) -> Result<(i64, Message), Error> {
        let mut listeners = self.listeners.lock().await;
        if listeners.is_empty() {
            return Err(Error::Config);
        }
        let futures = listeners
            .iter_mut()
            .map(|(id, listener)| {
                async move {
                    let message = listener.listener.next().await.ok_or(Error::Github)??;
                    listener.pending = message.message_id;
                    if message.message_id.is_none() {
                        listener.acked = None;
                    }
                    Ok((*id, message))
                }
                .boxed()
            })
            .collect::<Vec<_>>();
        select_all(futures).await.0
    }
    /// Ack only after the entire batch has reached durable handling.
    pub async fn acknowledge(&self, set: i64, id: i64) -> Result<(), Error> {
        let mut listeners = self.listeners.lock().await;
        let delivery = listeners.get_mut(&set).ok_or(Error::Config)?;
        if delivery.pending != Some(id) && delivery.acked == Some(id) {
            return Ok(());
        }
        match delivery.listener.ack(id).await {
            Ok(()) => {
                delivery.pending = None;
                delivery.acked = Some(id);
                Ok(())
            }
            Err(runwell_scaleset::Error::SessionRecreated) => {
                delivery.pending = None;
                delivery.acked = None;
                Err(Error::SessionReset)
            }
            Err(error) => Err(error.into()),
        }
    }
    /// Whether the delivery still belongs to the active session generation.
    pub async fn batch_pending(&self, set: i64, id: i64) -> bool {
        self.listeners
            .lock()
            .await
            .get(&set)
            .is_some_and(|d| d.pending == Some(id))
    }

    /// Update realizable maximum capacity for each next poll.
    pub async fn capacity(&self, set: i64, capacity: u32) {
        if let Some(listener) = self.listeners.lock().await.get(&set) {
            listener.listener.set_max_capacity(capacity);
        }
    }
    /// Close every session even if an earlier DELETE fails.
    pub async fn close(&self) -> Result<(), Error> {
        let listeners = std::mem::take(&mut *self.listeners.lock().await);
        let mut result = Ok(());
        for listener in listeners.into_values() {
            if listener.listener.close().await.is_err() {
                result = Err(Error::Github);
            }
        }
        result
    }
}
impl RunnerApi for GithubGateway {
    fn acquire(&self, set: i64, request: i64) -> NodeFuture<'_, bool> {
        Box::pin(async move {
            let mut listeners = self.listeners.lock().await;
            let delivery = listeners.get_mut(&set).ok_or(Error::Config)?;
            let result = delivery.listener.session().acquire_jobs(&[request]).await;
            if matches!(result, Err(runwell_scaleset::Error::SessionRecreated)) {
                // The old generation cannot be acked. Let Listener reset its
                // delivery and publish the recreated session's initial statistics.
                if let Some(id) = delivery.pending {
                    let _ = delivery.listener.ack(id).await;
                }
                delivery.pending = None;
                delivery.acked = None;
                return Err(Error::SessionReset);
            }
            Ok(result?.contains(&request))
        })
    }
    fn create<'a>(&'a self, set: i64, name: &'a str) -> NodeFuture<'a, Registration> {
        Box::pin(async move {
            let result = self
                .client
                .generate_jit_config(
                    set,
                    &JitSettings {
                        name: name.into(),
                        work_folder: "_work".into(),
                    },
                )
                .await?;
            if result.runner.id <= 0
                || result.runner.name != name
                || result.runner.runner_scale_set_id != set
            {
                return Err(Error::Github);
            }
            Ok(Registration {
                agent_id: result.runner.id,
                jit: SecretString::from(result.encoded_jit_config.expose().to_owned()),
            })
        })
    }
    fn lookup<'a>(&'a self, name: &'a str) -> NodeFuture<'a, Option<RunnerReference>> {
        Box::pin(async move { Ok(self.client.get_runner_by_name(name).await?) })
    }
    fn delete(&self, agent: i64) -> NodeFuture<'_, bool> {
        Box::pin(async move {
            Ok(runwell_runner::unregister(&self.client, agent).await?
                == RemoveRunnerResult::SafeToKill)
        })
    }
}
