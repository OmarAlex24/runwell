//! Owned message sessions and single-flight queue-token renewal.
use crate::{
    Error, Secret,
    api::{ActionsClient, SCALE_SETS},
    config::parse_url,
    events::{Envelope, Message},
    retry::{Clock, Policy},
    transport::{Request, Response, join, query},
    types::{List, Statistics},
};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Mutex;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionData {
    session_id: String,
    message_queue_url: Secret,
    message_queue_access_token: Secret,
    statistics: Option<Statistics>,
    #[serde(skip)]
    generation: u64,
}
impl SessionData {
    fn validate(&self) -> Result<(), Error> {
        uuid::Uuid::parse_str(&self.session_id)
            .map_err(|_| Error::protocol("sessions", "invalid session ID"))?;
        parse_url(self.message_queue_url.expose())?;
        if self.message_queue_access_token.expose().is_empty() {
            return Err(Error::protocol("sessions", "empty queue token"));
        }
        Ok(())
    }
}

/// A single owned session. Call [`Session::close`] on every graceful exit. Drop
/// schedules best-effort cleanup when a Tokio runtime exists; runtime teardown or
/// process termination cannot guarantee delivery of an asynchronous DELETE.
#[must_use = "close the session explicitly before shutting down the runtime"]
pub struct Session {
    pub(crate) core: Arc<Core>,
}
pub(crate) struct Core {
    client: ActionsClient,
    id: i64,
    owner: String,
    state: Mutex<Arc<SessionData>>,
    refresh: Mutex<()>,
    closed: AtomicBool,
}
#[derive(Clone)]
enum Operation {
    Poll {
        last: i64,
        capacity: u32,
        generation: u64,
    },
    Ack {
        id: i64,
        generation: u64,
    },
    Acquire(Vec<i64>),
}
impl Session {
    pub(crate) async fn open(client: ActionsClient, id: i64, owner: String) -> Result<Self, Error> {
        let data = create(&client, id, &owner).await?;
        Ok(Self {
            core: Arc::new(Core {
                client,
                id,
                owner,
                state: Mutex::new(Arc::new(data)),
                refresh: Mutex::new(()),
                closed: AtomicBool::new(false),
            }),
        })
    }
    /// Current absolute startup statistics. Missing statistics are rejected during creation.
    pub async fn statistics(&self) -> Statistics {
        self.core.initial().await.statistics.unwrap_or_default()
    }
    /// Explicitly renew queue credentials via PATCH, replacing both token and URL.
    pub async fn refresh(&self) -> Result<(), Error> {
        let previous = self.core.state.lock().await.clone();
        self.core.recover(&previous, false).await
    }
    /// Acquire available job IDs using the queue credential. Safe to repeat; returns
    /// only the IDs acquired. On session recreation, reprocess startup statistics.
    pub async fn acquire_jobs(&self, ids: &[i64]) -> Result<Vec<i64>, Error> {
        self.core.acquire_jobs(ids).await
    }
    /// Delete this session with a 30-second shutdown deadline. A 404 is success.
    /// Call even when polling, processing, acquisition or acknowledgment failed.
    pub async fn close(self) -> Result<(), Error> {
        self.core.close().await
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.core.closed.load(Ordering::Acquire) {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let core = self.core.clone();
                runtime.spawn(async move {
                    if let Err(error) = core.close().await {
                        tracing::warn!(%error, "session cleanup failed");
                    }
                });
            } else {
                tracing::warn!("session dropped without close outside a Tokio runtime");
            }
        }
    }
}

async fn create(client: &ActionsClient, id: i64, owner: &str) -> Result<SessionData, Error> {
    let transport = &client.inner.transport;
    let start = transport.clock.now();
    let mut spent = std::time::Duration::ZERO;
    let mut attempt = 0;
    let mut invalid = 0;
    let path = format!("{SCALE_SETS}/{id}/sessions");
    loop {
        let response = client
            .admin_request(
                Method::POST,
                &path,
                &[],
                Some(serde_json::json!({"ownerName": owner})),
                Policy::Never,
            )
            .await?;
        if response.status == StatusCode::CONFLICT {
            let elapsed = transport
                .clock
                .now()
                .duration_since(start)
                .unwrap_or_default()
                .max(spent);
            let remaining = client.inner.conflict_timeout.saturating_sub(elapsed);
            if remaining.is_zero() {
                return Err(response.error());
            }
            let delay = transport
                .retry
                .delay(attempt)
                .max(std::time::Duration::from_millis(1))
                .min(remaining);
            transport.clock.sleep(delay).await;
            spent = spent.saturating_add(delay);
            attempt = attempt.saturating_add(1);
            continue;
        }
        let data: SessionData = response.require(&[StatusCode::OK])?.json()?;
        // Validate the identity before using it as a path segment, including cleanup.
        uuid::Uuid::parse_str(&data.session_id)
            .map_err(|_| Error::protocol(&path, "invalid session ID"))?;
        let validation = data.validate().and_then(|()| {
            data.statistics
                .as_ref()
                .map(|_| ())
                .ok_or_else(|| Error::protocol(&path, "missing initial statistics"))
        });
        if let Err(error) = validation {
            delete(client, id, &data.session_id).await?;
            if invalid >= transport.retry.max_retries {
                return Err(error);
            }
            transport.clock.sleep(transport.retry.delay(invalid)).await;
            invalid += 1;
            continue;
        }
        return Ok(data);
    }
}
async fn delete(client: &ActionsClient, id: i64, session: &str) -> Result<(), Error> {
    client
        .admin_request(
            Method::DELETE,
            &format!("{SCALE_SETS}/{id}/sessions/{session}"),
            &[],
            None,
            Policy::Idempotent,
        )
        .await?
        .require(&[StatusCode::NO_CONTENT, StatusCode::NOT_FOUND])?;
    Ok(())
}
impl Core {
    pub(crate) fn clock(&self) -> &dyn Clock {
        self.client.inner.transport.clock.as_ref()
    }

    pub async fn initial(&self) -> Message {
        let data = self.state.lock().await;
        Message::initial(data.statistics.clone().unwrap_or_default(), data.generation)
    }

    async fn recover(&self, previous: &Arc<SessionData>, gone: bool) -> Result<(), Error> {
        let _guard = self.refresh.lock().await;
        let current = self.state.lock().await.clone();
        if !Arc::ptr_eq(previous, &current) {
            return Ok(());
        }
        let mut recreated = gone;
        let mut data = if gone {
            create(&self.client, self.id, &self.owner).await?
        } else {
            let response = self
                .client
                .admin_request(
                    Method::PATCH,
                    &format!("{SCALE_SETS}/{}/sessions/{}", self.id, current.session_id),
                    &[],
                    None,
                    Policy::Idempotent,
                )
                .await?;
            if response.status == StatusCode::NOT_FOUND {
                recreated = true;
                create(&self.client, self.id, &self.owner).await?
            } else {
                let mut refreshed: SessionData = response.require(&[StatusCode::OK])?.json()?;
                refreshed.validate()?;
                // PATCH can omit statistics; retain the last known startup snapshot.
                if refreshed.statistics.is_none() {
                    refreshed.statistics = current.statistics.clone();
                }
                refreshed
            }
        };
        recreated |= data.session_id != current.session_id;
        data.generation = current.generation + u64::from(recreated);
        *self.state.lock().await = Arc::new(data);
        Ok(())
    }

    async fn queue(&self, operation: Operation) -> Result<(Response, u64), Error> {
        let mut data = self.state.lock().await.clone();
        for attempt in 0..=1 {
            if let Operation::Ack { generation, .. } | Operation::Poll { generation, .. } =
                &operation
                && *generation != data.generation
            {
                return Err(Error::SessionRecreated);
            }
            let mut request = match &operation {
                Operation::Poll { last, capacity, .. } => {
                    let mut url = parse_url(data.message_queue_url.expose())?;
                    // Brief intentionally differs from Go HEAD: always send zero on startup.
                    query(&mut url, "lastMessageId", &last.to_string());
                    let mut request = Request::new(Method::GET, url, Policy::Idempotent);
                    request.capacity = Some(*capacity);
                    request
                }
                Operation::Ack { id, .. } => Request::new(
                    Method::DELETE,
                    join(
                        &parse_url(data.message_queue_url.expose())?,
                        &id.to_string(),
                    ),
                    Policy::Idempotent,
                ),
                Operation::Acquire(_) => Request::new(
                    Method::POST,
                    self.client
                        .service_url(&format!("{SCALE_SETS}/{}/acquirejobs", self.id))
                        .await?,
                    Policy::Idempotent,
                ),
            };
            if let Operation::Acquire(ids) = &operation {
                request.body = Some(serde_json::json!(ids));
            }
            let response = self
                .client
                .inner
                .transport
                .send(&request, "Bearer", &data.message_queue_access_token)
                .await?;
            if response.status == StatusCode::NOT_FOUND {
                self.recover(&data, true).await?;
                return Err(Error::SessionRecreated);
            }
            if response.status != StatusCode::UNAUTHORIZED || attempt == 1 {
                return Ok((response, data.generation));
            }
            self.recover(&data, false).await?;
            let next = self.state.lock().await.clone();
            if next.generation != data.generation {
                return Err(Error::SessionRecreated);
            }
            data = next;
        }
        Err(Error::protocol("queue", "authentication retry exhausted"))
    }

    pub async fn poll(
        &self,
        last: i64,
        capacity: u32,
        generation: u64,
    ) -> Result<Option<Message>, Error> {
        let (response, generation) = match self
            .queue(Operation::Poll {
                last,
                capacity,
                generation,
            })
            .await
        {
            Err(Error::SessionRecreated) => return Ok(Some(self.initial().await)),
            result => result?,
        };
        let response = response.require(&[StatusCode::OK, StatusCode::ACCEPTED])?;
        if response.status == StatusCode::ACCEPTED {
            return Ok(None);
        }
        response
            .json::<Envelope>()?
            .decode(&response.endpoint, generation)
            .map(Some)
    }
    pub async fn ack(&self, id: i64, generation: u64) -> Result<(), Error> {
        self.queue(Operation::Ack { id, generation })
            .await?
            .0
            .require(&[StatusCode::NO_CONTENT])?;
        Ok(())
    }
    pub async fn acquire_jobs(&self, ids: &[i64]) -> Result<Vec<i64>, Error> {
        self.queue(Operation::Acquire(ids.to_vec()))
            .await?
            .0
            .require(&[StatusCode::OK])?
            .json::<List<i64>>()?
            .values("acquirejobs")
    }
    async fn close(&self) -> Result<(), Error> {
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let _guard = self.refresh.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return Ok(());
            }
            let data = self.state.lock().await.clone();
            delete(&self.client, self.id, &data.session_id).await?;
            self.closed.store(true, Ordering::Release);
            Ok(())
        })
        .await;
        result.map_err(|_| Error::protocol("sessions", "shutdown deadline exceeded"))?
    }
}
