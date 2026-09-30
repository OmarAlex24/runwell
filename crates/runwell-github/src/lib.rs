//! GitHub REST access and GitHub App or PAT authentication for runwell.
//!
//! Credentials must never enter logs. App JWTs use RS256 with bounded lifetime;
//! installation tokens and Actions-service credentials have separate lifecycles.

#![deny(missing_docs)]

use secrecy::SecretString;

/// A GitHub credential kept out of ordinary debug output.
pub enum Auth {
    /// A personal access token.
    Pat(SecretString),
    /// GitHub App identity and signing material.
    App {
        /// App identifier used as JWT issuer.
        app_id: u64,
        /// Installation whose token will be requested.
        installation_id: u64,
        /// PEM-encoded private key.
        private_key: SecretString,
    },
}

/// GitHub App token signing boundary.
pub trait AppJwtSigner {
    /// Sign an RS256 JWT with iat offset for skew and expiration within ten minutes.
    fn sign(
        &self,
        app_id: u64,
        private_key: &SecretString,
        now_unix: u64,
    ) -> Result<SecretString, Error>;
}

/// REST client boundary; endpoint calls are deferred beyond M0.
pub struct RestClient {
    /// HTTPS API base, separate from Actions-service URLs.
    pub api_base: reqwest::Url,
    /// Authentication credential, never formatted for logging.
    pub auth: Auth,
}

impl RestClient {
    /// Exchange an App JWT for an installation token; currently unimplemented.
    pub async fn installation_token(&self) -> Result<SecretString, Error> {
        Err(Error::Unimplemented)
    }

    /// Read a REST resource; currently unimplemented and makes no request.
    pub async fn get(&self, _path: &str) -> Result<serde_json::Value, Error> {
        Err(Error::Unimplemented)
    }
}

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
