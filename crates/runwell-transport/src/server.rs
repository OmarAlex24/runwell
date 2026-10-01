use crate::{Error, Handler, Identity, Response, Tls, dispatch::Dispatcher, tls::authorized};
use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, Full, Limited, StreamBody, combinators::UnsyncBoxBody};
use hyper::{Request as HttpRequest, Response as HttpResponse, body::Frame, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::{collections::BTreeSet, convert::Infallible, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

type Body = UnsyncBoxBody<Bytes, Infallible>;
/// Serve only authorized peers; detached accepted calls finish even if the caller
/// times out. Shutdown stops transport tasks, never host units or reservations.
pub async fn serve(
    listener: TcpListener,
    tls: &Tls,
    allowed: BTreeSet<Identity>,
    handler: Arc<dyn Handler>,
    shutdown: CancellationToken,
) -> Result<(), Error> {
    let acceptor = TlsAcceptor::from(tls.server.clone());
    let permits = Arc::new(tokio::sync::Semaphore::new(128));
    let dispatcher = Dispatcher::new(handler, 32);
    loop {
        let (tcp, _) = tokio::select! { _ = shutdown.cancelled() => return Ok(()), result = listener.accept() => result.map_err(|_| Error::Io)? };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            continue;
        };
        let acceptor = acceptor.clone();
        let allowed = allowed.clone();
        let dispatcher = dispatcher.clone();
        let stop = shutdown.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let Ok(Ok(tls)) =
                tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await
            else {
                return;
            };
            let Ok(peer) = authorized(tls.get_ref().1.peer_certificates(), &allowed) else {
                return;
            };
            let service =
                service_fn(move |request| dispatch(request, peer.clone(), dispatcher.clone()));
            let connection = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                .max_concurrent_streams(32)
                .serve_connection(TokioIo::new(tls), service);
            tokio::select! { _ = stop.cancelled() => {}, _ = connection => {} }
        });
    }
}
fn response(status: u16, data: Vec<u8>) -> HttpResponse<Body> {
    let mut response = HttpResponse::new(Full::new(Bytes::from(data)).boxed_unsync());
    *response.status_mut() =
        hyper::StatusCode::from_u16(status).unwrap_or(hyper::StatusCode::INTERNAL_SERVER_ERROR);
    response
}
async fn dispatch<B: hyper::body::Body<Data = Bytes>>(
    request: HttpRequest<B>,
    peer: Identity,
    dispatcher: Arc<Dispatcher>,
) -> Result<HttpResponse<Body>, Infallible>
where
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    // Reserve before reading a body, including streams; the permit remains held
    // by any detached handler even after its originating HTTP stream disappears.
    let Ok(permit) = dispatcher.reserve() else {
        return Ok(response(429, vec![]));
    };
    if request.method() == hyper::Method::GET && request.uri().path() == "/v1/events" {
        let events = stream::unfold(
            (dispatcher, peer, permit),
            |(dispatcher, peer, permit)| async move {
                let Ok(Response::Report(report)) = dispatcher.report(peer.clone()).await else {
                    return None;
                };
                let Ok(mut bytes) = serde_json::to_vec(&report) else {
                    return None;
                };
                bytes.push(b'\n');
                tokio::time::sleep(Duration::from_secs(1)).await;
                Some((
                    Ok::<_, Infallible>(Frame::data(Bytes::from(bytes))),
                    (dispatcher, peer, permit),
                ))
            },
        );
        return Ok(HttpResponse::new(StreamBody::new(events).boxed_unsync()));
    }
    if request.method() != hyper::Method::POST || request.uri().path() != "/v1/rpc" {
        return Ok(response(404, vec![]));
    }
    let result = async {
        let bytes = tokio::time::timeout(
            Duration::from_secs(10),
            Limited::new(request.into_body(), 4 * 1024 * 1024).collect(),
        )
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|_| Error::Protocol)?
        .to_bytes();
        let request = serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
        drop(bytes);
        let result = dispatcher.call(peer, request, permit).await?;
        serde_json::to_vec(&result).map_err(|_| Error::Protocol)
    }
    .await;
    Ok(match result {
        Ok(bytes) => response(200, bytes),
        Err(Error::Unauthorized) => response(403, vec![]),
        Err(Error::Protocol) => response(400, vec![]),
        Err(Error::Uncertain) => response(409, vec![]),
        Err(Error::Busy) => response(429, vec![]),
        Err(Error::Timeout) => response(504, vec![]),
        Err(_) => response(503, vec![]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct NeverRead;
    impl hyper::body::Body for NeverRead {
        type Data = Bytes;
        type Error = Infallible;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<Frame<Bytes>, Infallible>>> {
            panic!("overloaded requests must not retain or read bodies");
        }
    }
    struct Unused;
    impl Handler for Unused {
        fn handle(&self, _: Identity, _: crate::Request) -> crate::RpcFuture<'_> {
            Box::pin(async { panic!("handler must not run") })
        }
    }
    #[tokio::test]
    async fn overload_is_rejected_before_polling_the_request_body() {
        let dispatcher = Dispatcher::new(Arc::new(Unused), 1);
        let _permit = dispatcher.reserve().unwrap();
        let request = HttpRequest::post("/v1/rpc").body(NeverRead).unwrap();
        let response = dispatch(request, Identity::controller("controller-1"), dispatcher)
            .await
            .unwrap();
        assert_eq!(response.status(), 429);
    }
}
