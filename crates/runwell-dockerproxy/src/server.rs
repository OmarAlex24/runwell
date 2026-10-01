use crate::{Error, Rewrite, Rewriter, route};
use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{Request, Response, StatusCode, body::Incoming, header, service::service_fn};
use hyper_util::rt::TokioIo;
use std::{
    convert::Infallible,
    future::Future,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{RwLock, oneshot},
    task::JoinHandle,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type Body = UnsyncBoxBody<Bytes, BoxError>;
fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}
struct State {
    rewrite: Rewriter,
    cancel: CancellationToken,
    accept_cancel: CancellationToken,
    closing: AtomicBool,
    mutations: RwLock<()>,
    tasks: TaskTracker,
}
impl State {
    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        let cancel = self.cancel.clone();
        self.tasks.spawn(async move {
            tokio::select! { biased; _ = cancel.cancelled() => {}, _ = future => {} }
        });
    }
}
/// Running proxy with explicit, awaited teardown of HTTP and upgraded streams.
/// Dropping closes connections but leaves the socket for restart recovery.
pub struct Proxy {
    state: Arc<State>,
    accept: Option<JoinHandle<()>>,
    socket: PathBuf,
}
impl Proxy {
    /// Bind a per-job socket. Its parent stays daemon-owned to prevent replacing
    /// the socket; the socket itself belongs to the configured job user/group.
    pub async fn start(rewrite: Rewriter) -> Result<Self, Error> {
        let socket = rewrite.spec.socket(&rewrite.settings);
        let listener = crate::socket::bind(&socket, rewrite.spec.uid, rewrite.spec.gid).await?;
        let state = Arc::new(State {
            rewrite,
            cancel: CancellationToken::new(),
            accept_cancel: CancellationToken::new(),
            closing: AtomicBool::new(false),
            mutations: RwLock::new(()),
            tasks: TaskTracker::new(),
        });
        let run = state.clone();
        let accept = tokio::spawn(async move { accept(listener, run).await });
        Ok(Self {
            state,
            accept: Some(accept),
            socket,
        })
    }
    /// Stop accepting and cancel/join every stream before deleting the socket.
    pub async fn shutdown(mut self) -> Result<(), Error> {
        self.state.closing.store(true, Ordering::SeqCst);
        self.state.accept_cancel.cancel();
        if let Some(task) = self.accept.take() {
            task.await.map_err(|_| Error::Io)?;
        }
        // Wait for accepted creates to receive daemon response headers, even if
        // their client disconnected. Otherwise cleanup could list before a late
        // daemon create commits. A timeout keeps durable cleanup pending.
        let drained = tokio::time::timeout(Duration::from_secs(30), self.state.mutations.write())
            .await
            .is_ok();
        self.state.cancel.cancel();
        self.state.tasks.close();
        self.state.tasks.wait().await;
        crate::socket::remove(&self.socket)?;
        if drained {
            Ok(())
        } else {
            Err(Error::Upstream)
        }
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.state.accept_cancel.cancel();
        self.state.cancel.cancel();
    }
}
async fn accept(listener: UnixListener, state: Arc<State>) {
    while let Some((stream, _)) =
        crate::accept::retry(|| listener.accept(), &state.accept_cancel).await
    {
        let service = state.clone();
        state.spawn(async move {
            let result = hyper::server::conn::http1::Builder::new()
                .keep_alive(true)
                .half_close(true)
                .preserve_header_case(true)
                .serve_connection(
                    TokioIo::new(stream),
                    service_fn(move |request| {
                        let state = service.clone();
                        async move {
                            Ok::<_, Infallible>(
                                dispatch(request, state)
                                    .await
                                    .unwrap_or_else(error_response),
                            )
                        }
                    }),
                )
                .with_upgrades()
                .await;
            if result.is_err() {
                tracing::debug!("Docker proxy downstream connection closed");
            }
        });
    }
}
fn asks_upgrade(request: &Request<Incoming>) -> bool {
    request.headers().contains_key(header::UPGRADE)
        || request
            .headers()
            .get_all(header::CONNECTION)
            .iter()
            .any(|header| {
                header.to_str().is_ok_and(|value| {
                    value
                        .split(',')
                        .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
                })
            })
}
async fn dispatch(request: Request<Incoming>, state: Arc<State>) -> Result<Response<Body>, Error> {
    let kind = route(request.method().as_str(), request.uri().path())?;
    if asks_upgrade(&request)
        && !crate::routes::upgrade_allowed(request.method().as_str(), request.uri().path())?
    {
        return Err(Error::Upgrade);
    }
    if kind.is_some_and(|kind| kind != Rewrite::Build) {
        let (send, receive) = oneshot::channel();
        let worker = state.clone();
        state.spawn(async move {
            let _ = send.send(forward(request, worker, kind).await);
        });
        receive.await.map_err(|_| Error::Upstream)?
    } else {
        forward(request, state, kind).await
    }
}
async fn forward(
    mut request: Request<Incoming>,
    state: Arc<State>,
    kind: Option<Rewrite>,
) -> Result<Response<Body>, Error> {
    let upgrade = request
        .headers()
        .contains_key(header::UPGRADE)
        .then(|| hyper::upgrade::on(&mut request));
    let _mutation = if kind.is_some_and(|kind| kind != Rewrite::Build) {
        Some(state.mutations.read().await)
    } else {
        None
    };
    if state.closing.load(Ordering::SeqCst) {
        return Err(Error::Stopping);
    }
    if kind == Some(Rewrite::Build) {
        let rewritten = state.rewrite.build_query(
            request
                .uri()
                .path_and_query()
                .ok_or(Error::Payload)?
                .as_str(),
        )?;
        *request.uri_mut() = rewritten.parse().map_err(|_| Error::Payload)?;
    }
    let (mut parts, incoming) = request.into_parts();
    let body = if let Some(kind) = kind.filter(|kind| *kind != Rewrite::Build) {
        if parts
            .headers
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v != "identity")
        {
            return Err(Error::Payload);
        }
        let limit = state.rewrite.settings.max_json_bytes;
        if parts
            .headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|v| v > limit as u64)
        {
            return Err(Error::BodyTooLarge(limit));
        }
        let bytes = tokio::time::timeout(
            Duration::from_secs(u64::from(state.rewrite.settings.body_read_seconds)),
            bounded(incoming, limit),
        )
        .await
        .map_err(|_| Error::BodyTimeout)??;
        let bytes = state.rewrite.json(kind, &bytes)?;
        parts.headers.remove(header::TRANSFER_ENCODING);
        parts.headers.remove(header::TRAILER);
        parts
            .headers
            .insert(header::CONTENT_LENGTH, bytes.len().into());
        full(bytes)
    } else {
        incoming.map_err(BoxError::from).boxed_unsync()
    };
    let request = Request::from_parts(parts, body);
    let stream = UnixStream::connect(&state.rewrite.settings.upstream_socket)
        .await
        .map_err(|_| Error::Upstream)?;
    let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
        .preserve_header_case(true)
        .handshake(TokioIo::new(stream))
        .await
        .map_err(|_| Error::Upstream)?;
    state.spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    let mut response = sender
        .send_request(request)
        .await
        .map_err(|_| Error::Upstream)?;
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        let downstream = upgrade.ok_or(Error::Upstream)?;
        let upstream = hyper::upgrade::on(&mut response);
        state.spawn(async move {
            match tokio::try_join!(downstream, upstream) {
                Ok((downstream, upstream)) => {
                    let _ = tokio::io::copy_bidirectional(
                        &mut TokioIo::new(downstream),
                        &mut TokioIo::new(upstream),
                    )
                    .await;
                }
                Err(_) => tracing::debug!("Docker proxy upgrade closed before handshake completed"),
            }
        });
    }
    Ok(response.map(|body| body.map_err(BoxError::from).boxed_unsync()))
}
async fn bounded(mut body: Incoming, limit: usize) -> Result<Bytes, Error> {
    let mut bytes = BytesMut::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| Error::Payload)?;
        if let Ok(data) = frame.into_data() {
            if data.len() > limit.saturating_sub(bytes.len()) {
                return Err(Error::BodyTooLarge(limit));
            }
            bytes.extend_from_slice(&data);
        }
    }
    Ok(bytes.freeze())
}
fn error_response(error: Error) -> Response<Body> {
    let status = match error {
        Error::BodyTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
        Error::Payload | Error::CgroupUpdate | Error::Path | Error::JsonKeys | Error::Upgrade => {
            StatusCode::BAD_REQUEST
        }
        Error::BodyTimeout => StatusCode::REQUEST_TIMEOUT,
        Error::HostAccess => StatusCode::FORBIDDEN,
        Error::Stopping => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_GATEWAY,
    };
    let mut response = Response::new(full(
        serde_json::json!({"message": error.to_string()}).to_string(),
    ));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    // Early rejection may leave an unread request body. Never reuse that stream.
    response.headers_mut().insert(
        header::CONNECTION,
        header::HeaderValue::from_static("close"),
    );
    response
}
