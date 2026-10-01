//! TLS 1.3 mutual authentication and idempotent node execution RPCs.
//! Wire bodies and foreign TLS errors are deliberately never formatted.
mod agent;
mod agent_ops;
pub mod certs;
mod client;
mod dispatch;
mod protocol;
mod reporting;
mod server;
mod tombstones;
pub use reporting::{DrainJournal, report_until_drained};
mod tls;
pub use agent::{Agent, Clock, WallClock};
pub use client::Client;
pub use protocol::*;
pub use server::serve;
use std::{future::Future, pin::Pin};
pub use tls::{Identity, Tls};

/// Sanitized transport errors. No request, response, certificate key or JIT body.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("transport I/O failed")]
    Io,
    #[error("TLS identity or trust validation failed")]
    Tls,
    #[error("peer is not authorized")]
    Unauthorized,
    #[error("invalid RPC or conflicting attempt")]
    Protocol,
    #[error("RPC deadline exceeded; outcome may be ambiguous")]
    Timeout,
    #[error("node operation failed; retry retained durable work")]
    Backend,
    #[error("start outcome is unresolved; inspect durable execution")]
    Uncertain,
    #[error("RPC handler capacity exhausted")]
    Busy,
}
impl From<runwell_node::Error> for Error {
    fn from(_: runwell_node::Error) -> Self {
        Self::Backend
    }
}
impl From<runwell_store::Error> for Error {
    fn from(_: runwell_store::Error) -> Self {
        Self::Backend
    }
}
/// Object-safe transport port for real mTLS and deterministic fault injection.
pub type RpcFuture<'a> = Pin<Box<dyn Future<Output = Result<Response, Error>> + Send + 'a>>;
/// Implementations serialize mutations; transport retries may repeat any request.
pub trait Rpc: Send + Sync {
    fn call(&self, request: Request) -> RpcFuture<'_>;
}
/// Authenticated receiver used for controller registration/reporting.
pub trait Handler: Send + Sync {
    fn handle(&self, peer: Identity, request: Request) -> RpcFuture<'_>;
}
