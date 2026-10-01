use crate::Error;
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;

/// Credential with no Debug implementation.
pub enum Auth {
    /// Personal access token.
    Pat(SecretString),
    /// App installation credentials.
    App {
        /// App identity.
        app_id: u64,
        /// Installation identity.
        installation_id: u64,
        /// PEM signing key.
        private_key: SecretString,
    },
}
impl Auth {
    /// Load the existing config's file/env credential references without logging.
    pub async fn from_config(config: &runwell_config::AuthConfig) -> Result<Self, Error> {
        use runwell_config::AuthConfig;
        let auth = match config {
            AuthConfig::Pat { token_file } => Self::Pat(secret(
                tokio::fs::read_to_string(token_file)
                    .await
                    .map_err(|_| Error::Auth)?,
            )?),
            AuthConfig::PatEnv { token_env } => {
                Self::Pat(secret(std::env::var(token_env).map_err(|_| Error::Auth)?)?)
            }
            AuthConfig::App {
                app_id,
                installation_id,
                private_key_file,
            } => Self::App {
                app_id: *app_id,
                installation_id: *installation_id,
                private_key: secret(
                    tokio::fs::read_to_string(private_key_file)
                        .await
                        .map_err(|_| Error::Auth)?,
                )?,
            },
            AuthConfig::AppEnv {
                app_id,
                installation_id,
                private_key_env,
            } => Self::App {
                app_id: *app_id,
                installation_id: *installation_id,
                private_key: secret(std::env::var(private_key_env).map_err(|_| Error::Auth)?)?,
            },
        };
        Ok(auth)
    }
}
fn secret(value: String) -> Result<SecretString, Error> {
    if value.trim().is_empty() {
        return Err(Error::Auth);
    }
    Ok(value.trim().to_owned().into())
}
/// App JWT signing boundary, injectable independently of REST.
pub trait AppJwtSigner {
    /// Sign an RS256 JWT valid for less than ten minutes, with skew allowance.
    fn sign(
        &self,
        app_id: u64,
        private_key: &SecretString,
        now_unix: u64,
    ) -> Result<SecretString, Error>;
}
/// Production RS256 signer.
pub struct Rs256Signer;
impl AppJwtSigner for Rs256Signer {
    fn sign(
        &self,
        app_id: u64,
        private_key: &SecretString,
        now_unix: u64,
    ) -> Result<SecretString, Error> {
        #[derive(Serialize)]
        struct Claims {
            iat: u64,
            exp: u64,
            iss: String,
        }
        if app_id == 0 {
            return Err(Error::Config);
        }
        let claims = Claims {
            iat: now_unix.saturating_sub(60),
            exp: now_unix.saturating_add(540),
            iss: app_id.to_string(),
        };
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key.expose_secret().as_bytes())
            .map_err(|_| Error::Auth)?;
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &key,
        )
        .map(SecretString::from)
        .map_err(|_| Error::Auth)
    }
}
