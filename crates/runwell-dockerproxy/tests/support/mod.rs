#![allow(dead_code)]
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{Request, Response, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use runwell_dockerproxy::*;
use std::{convert::Infallible, future::Future, path::Path, sync::Arc};
use tokio::{
    net::{UnixListener, UnixStream},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Body = UnsyncBoxBody<Bytes, BoxError>;
pub fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub settings: DockerProxyConfig,
    pub spec: ProxySpec,
    pub proxy: Option<Proxy>,
    cancel: CancellationToken,
    upstream: Option<tokio::task::JoinHandle<()>>,
}
impl Fixture {
    pub async fn new<F, Fut>(handler: F) -> Self
    where
        F: Fn(Request<Incoming>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response<Body>> + Send + 'static,
    {
        // Canonicalize macOS /var -> /private/var before the anti-symlink checks.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let settings = DockerProxyConfig {
            upstream_socket: root.join("up.sock"),
            run_dir: root.join("run"),
            ..Default::default()
        };
        let spec = ProxySpec {
            job_id: 42,
            node: "node-a".into(),
            cgroup_parent: "ci-rw-j42.slice".into(),
            memory_max: Some(1024),
            uid: rustix::process::geteuid().as_raw(),
            gid: rustix::process::getegid().as_raw(),
        };
        let listener = UnixListener::bind(&settings.upstream_socket).unwrap();
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let handler = Arc::new(handler);
        let upstream = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    result = listener.accept() => {
                        let (stream, _) = result.unwrap();
                        let handler = handler.clone();
                        connections.spawn(async move {
                            let _ = hyper::server::conn::http1::Builder::new().half_close(true)
                                .serve_connection(TokioIo::new(stream), service_fn(move |request| {
                                    let handler = handler.clone();
                                    async move { Ok::<_, Infallible>(handler(request).await) }
                                })).with_upgrades().await;
                        });
                    }
                    Some(_) = connections.join_next(), if !connections.is_empty() => {},
                }
            }
            connections.abort_all();
        });
        Self {
            dir,
            settings,
            spec,
            proxy: None,
            cancel,
            upstream: Some(upstream),
        }
    }
    pub async fn stop_upstream(&mut self) {
        self.cancel.cancel();
        self.upstream.take().unwrap().await.unwrap();
    }
    pub async fn start(&mut self) {
        self.proxy = Some(
            Proxy::start(
                Rewriter::new(
                    self.spec.clone(),
                    self.settings.clone(),
                    CgroupDriver::Systemd,
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        );
    }
    pub fn socket(&self) -> std::path::PathBuf {
        self.spec.socket(&self.settings)
    }
    pub async fn send(&self, method: &str, path: &str, body: Body) -> Response<Incoming> {
        send(
            &self.socket(),
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "docker")
                .header("te", "trailers")
                .body(body)
                .unwrap(),
        )
        .await
    }
    pub async fn stop(&mut self) {
        self.proxy.take().unwrap().shutdown().await.unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
pub async fn send(socket: &Path, request: Request<Body>) -> Response<Incoming> {
    let stream = UnixStream::connect(socket).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    sender.send_request(request).await.unwrap()
}
pub async fn bytes(response: Response<Incoming>) -> Bytes {
    response.into_body().collect().await.unwrap().to_bytes()
}
pub async fn echo(request: Request<Incoming>) -> Response<Body> {
    let uri = request.uri().to_string();
    let (parts, body) = request.into_parts();
    let mut response = Response::new(body.map_err(BoxError::from).boxed_unsync());
    response.headers_mut().insert("x-uri", uri.parse().unwrap());
    if let Some(v) = parts.headers.get("x-exact") {
        response.headers_mut().insert("x-exact", v.clone());
    }
    response
}
