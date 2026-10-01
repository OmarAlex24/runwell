use crate::{Error, Identity, Request, Response, Rpc, RpcFuture, Tls};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request as HttpRequest, client::conn::http2::SendRequest};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpStream, sync::Mutex};
use tokio_rustls::TlsConnector;

/// Reusable HTTP/2 connection with bounded deadlines and exponential reconnect.
pub struct Client {
    address: String,
    peer: Identity,
    tls: Arc<rustls::ClientConfig>,
    sender: Mutex<Option<SendRequest<Full<Bytes>>>>,
    deadline: Duration,
    attempts: u32,
}
impl Client {
    pub fn new(
        address: String,
        peer: Identity,
        tls: &Tls,
        deadline: Duration,
        attempts: u32,
    ) -> Result<Self, Error> {
        peer.validate()?;
        if deadline.is_zero() || !(1..=8).contains(&attempts) {
            return Err(Error::Protocol);
        }
        Ok(Self {
            address,
            peer,
            tls: tls.client.clone(),
            sender: Mutex::new(None),
            deadline,
            attempts,
        })
    }
    async fn connect(&self) -> Result<SendRequest<Full<Bytes>>, Error> {
        let tcp = TcpStream::connect(&self.address)
            .await
            .map_err(|_| Error::Io)?;
        tcp.set_nodelay(true).map_err(|_| Error::Io)?;
        let name =
            rustls::pki_types::ServerName::try_from(self.peer.dns()).map_err(|_| Error::Tls)?;
        let tls = TlsConnector::from(self.tls.clone())
            .connect(name, tcp)
            .await
            .map_err(|_| Error::Tls)?;
        let cert = tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|c| c.first())
            .ok_or(Error::Tls)?;
        if Identity::certificate(cert)? != self.peer
            || tls.get_ref().1.alpn_protocol() != Some(b"h2")
        {
            return Err(Error::Unauthorized);
        }
        let (sender, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
                .await
                .map_err(|_| Error::Io)?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(sender)
    }
    pub(crate) async fn send(
        &self,
        request: HttpRequest<Full<Bytes>>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, Error> {
        let mut slot = self.sender.lock().await;
        if slot.as_ref().is_none_or(|s| s.is_closed()) {
            *slot = Some(self.connect().await?);
        }
        let mut sender = slot.as_ref().ok_or(Error::Io)?.clone();
        drop(slot);
        let response = sender.send_request(request).await.map_err(|_| Error::Io)?;
        if !response.status().is_success() {
            return Err(match response.status().as_u16() {
                400 => Error::Protocol,
                403 => Error::Unauthorized,
                409 => Error::Uncertain,
                429 => Error::Busy,
                504 => Error::Timeout,
                _ => Error::Backend,
            });
        }
        Ok(response)
    }
    async fn once(&self, bytes: Vec<u8>) -> Result<Response, Error> {
        let request = HttpRequest::post("https://runwell/v1/rpc")
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(bytes)))
            .map_err(|_| Error::Protocol)?;
        let response = self.send(request).await?;
        let bytes = Limited::new(response.into_body(), 4 * 1024 * 1024)
            .collect()
            .await
            .map_err(|_| Error::Protocol)?
            .to_bytes();
        serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)
    }
    /// Receive an actual NDJSON HTTP/2 stream of state snapshots. Reconnection
    /// resumes with a complete snapshot, so missed intermediate events are safe.
    pub async fn watch(
        &self,
        updates: tokio::sync::mpsc::Sender<crate::Report>,
    ) -> Result<(), Error> {
        let request = HttpRequest::get("https://runwell/v1/events")
            .body(Full::new(Bytes::new()))
            .map_err(|_| Error::Protocol)?;
        let response = tokio::time::timeout(self.deadline, self.send(request))
            .await
            .map_err(|_| Error::Timeout)??;
        let mut body = response.into_body();
        let mut pending = Vec::new();
        while let Some(frame) = tokio::time::timeout(self.deadline, body.frame())
            .await
            .map_err(|_| Error::Timeout)?
        {
            if let Ok(data) = frame.map_err(|_| Error::Io)?.into_data() {
                pending.extend_from_slice(&data);
                if pending.len() > 4 * 1024 * 1024 {
                    return Err(Error::Protocol);
                }
                while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                    let report: crate::Report =
                        serde_json::from_slice(&pending[..end]).map_err(|_| Error::Protocol)?;
                    if report.node_id != self.peer.id {
                        return Err(Error::Unauthorized);
                    }
                    pending.drain(..=end);
                    updates.send(report).await.map_err(|_| Error::Io)?;
                }
            }
        }
        Err(Error::Io)
    }
}
impl Rpc for Client {
    fn call(&self, request: Request) -> RpcFuture<'_> {
        Box::pin(async move {
            let body = serde_json::to_vec(&request).map_err(|_| Error::Protocol)?;
            let mut last = Error::Io;
            for attempt in 0..self.attempts {
                let result = tokio::time::timeout(self.deadline, self.once(body.clone()))
                    .await
                    .unwrap_or(Err(Error::Timeout));
                match result {
                    Ok(response) => return Ok(response),
                    Err(
                        error @ (Error::Tls
                        | Error::Unauthorized
                        | Error::Protocol
                        | Error::Backend
                        | Error::Uncertain),
                    ) => return Err(error),
                    Err(error) => last = error,
                }
                *self.sender.lock().await = None;
                if attempt + 1 < self.attempts {
                    tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
                }
            }
            Err(last)
        })
    }
}
