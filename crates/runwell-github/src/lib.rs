//! GitHub REST access with App/PAT auth and bounded 401 recovery for rerun requests.
//! No error includes credentials, request URLs, or response bodies.
#![deny(missing_docs)]
mod auth;
mod client;
pub use auth::{AppJwtSigner, Auth, Rs256Signer};
pub use client::{RestClient, RunJob, RunState};

/// Sanitized REST errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid endpoint, repository, identity, or credential configuration.
    #[error("invalid GitHub REST configuration")]
    Config,
    /// Credential could not be loaded or signed.
    #[error("GitHub authentication failed")]
    Auth,
    /// A transport failure; a POST may already have reached GitHub.
    #[error("GitHub transport outcome is uncertain")]
    Transport,
    /// HTTP status only; response contents are not exposed.
    #[error("GitHub returned HTTP {0}")]
    Status(u16),
    /// An incomplete or malformed API response.
    #[error("invalid GitHub REST response")]
    Response,
}
impl Error {
    /// Whether a mutating request might have taken effect and must not be resent.
    pub fn ambiguous(&self) -> bool {
        matches!(self, Self::Transport | Self::Response)
            || matches!(self, Self::Status(code) if *code >= 500)
    }
}
