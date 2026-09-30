//! PAT and GitHub App authentication, with single-flight admin-token refresh.
use crate::{
    Error, Secret,
    config::{Config, parse_url},
    retry::Policy,
    transport::{Request, Transport, join},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{Method, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::UNIX_EPOCH};
use tokio::sync::Mutex;

/// GitHub credentials. All key and token material is redacted on formatting.
#[derive(Clone, Debug)]
pub enum Credentials {
    /// Personal access token for repo, org or enterprise scope.
    Pat(Secret),
    /// GitHub App installed on the target repository or organization.
    App {
        /// App client ID or numeric App ID (JWT issuer).
        client_id: String,
        /// Installation ID.
        installation_id: u64,
        /// RSA private key in PEM format.
        private_key: Secret,
    },
}

#[derive(Clone)]
pub(crate) struct Admin {
    pub url: Url,
    pub token: Secret,
    exp: u64,
}
pub(crate) struct TokenManager {
    transport: Arc<Transport>,
    config_url: String,
    api: Url,
    registration_path: String,
    credentials: Credentials,
    cached: Mutex<Option<Arc<Admin>>>,
}
#[derive(Deserialize)]
struct Token {
    token: Secret,
}
#[derive(Deserialize)]
struct Connection {
    url: String,
    token: Secret,
}
#[derive(Deserialize)]
struct Expiry {
    exp: u64,
}

impl TokenManager {
    pub fn new(config: &Config, transport: Arc<Transport>) -> Self {
        Self {
            transport,
            config_url: config.config_url.clone(),
            api: config.github_api_url.clone(),
            registration_path: config.registration_path.clone(),
            credentials: config.credentials.clone(),
            cached: Mutex::new(None),
        }
    }
    /// Unlike Go HEAD e6daac7, rejected admin tokens are refreshed immediately.
    /// Pointer identity deduplicates concurrent 401s even if the service returns the same JWT.
    pub async fn admin(&self, rejected: Option<&Arc<Admin>>) -> Result<Arc<Admin>, Error> {
        let mut cached = self.cached.lock().await;
        let now = self
            .transport
            .clock
            .now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Some(admin) = &*cached {
            let rejected_current = rejected.is_some_and(|old| Arc::ptr_eq(old, admin));
            if !rejected_current && now.saturating_add(60) < admin.exp {
                return Ok(admin.clone());
            }
        }
        let admin = Arc::new(self.exchange(now).await?);
        *cached = Some(admin.clone());
        Ok(admin)
    }

    #[tracing::instrument(skip_all)]
    async fn exchange(&self, now: u64) -> Result<Admin, Error> {
        let credential = match &self.credentials {
            Credentials::Pat(pat) => pat.clone(),
            Credentials::App {
                client_id,
                installation_id,
                private_key,
            } => {
                #[derive(Serialize)]
                struct Claims<'a> {
                    iss: &'a str,
                    iat: u64,
                    exp: u64,
                }
                let key = EncodingKey::from_rsa_pem(private_key.expose().as_bytes())
                    .map_err(|_| Error::Config("invalid App RSA private key"))?;
                let jwt = jsonwebtoken::encode(
                    &Header::new(Algorithm::RS256),
                    &Claims {
                        iss: client_id,
                        iat: now.saturating_sub(60),
                        exp: now.saturating_add(480),
                    },
                    &key,
                )
                .map_err(|_| Error::Config("failed to sign App JWT"))?;
                let mut request = Request::new(
                    Method::POST,
                    join(
                        &self.api,
                        &format!("app/installations/{installation_id}/access_tokens"),
                    ),
                    Policy::Never,
                );
                request.content_type = "application/vnd.github+json";
                self.transport
                    .send(&request, "Bearer", &Secret::new(jwt))
                    .await?
                    .require(&[StatusCode::CREATED])?
                    .json::<Token>()?
                    .token
            }
        };
        let mut registration = Request::new(
            Method::POST,
            join(&self.api, &self.registration_path),
            Policy::Never,
        );
        registration.content_type = "application/vnd.github.v3+json";
        let token: Token = self
            .transport
            .send(&registration, "Bearer", &credential)
            .await?
            .require(&[StatusCode::CREATED])?
            .json()?;
        if token.token.expose().is_empty() {
            return Err(Error::protocol(
                registration.url.path(),
                "empty registration token",
            ));
        }
        let mut request = Request::new(
            Method::POST,
            join(&self.api, "actions/runner-registration"),
            Policy::RegistrationPropagation,
        );
        request.body = Some(serde_json::json!({"url":self.config_url,"runner_event":"register"}));
        let response = self
            .transport
            .send(&request, "RemoteAuth", &token.token)
            .await?;
        if !response.status.is_success() {
            return Err(response.error());
        }
        let connection: Connection = response.json()?;
        let claims = connection
            .token
            .expose()
            .split('.')
            .nth(1)
            .and_then(|payload| URL_SAFE_NO_PAD.decode(payload).ok())
            .and_then(|payload| serde_json::from_slice::<Expiry>(&payload).ok())
            .ok_or_else(|| {
                Error::protocol(request.url.path(), "admin JWT is missing a valid exp claim")
            })?;
        // The token is issued over TLS; exp is scheduling metadata, not an authorization decision.
        let url = parse_url(&connection.url)?;
        Ok(Admin {
            url,
            token: connection.token,
            exp: claims.exp,
        })
    }
}
