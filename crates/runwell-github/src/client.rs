use crate::{AppJwtSigner, Auth, Error, Rs256Signer};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::Mutex;

/// Latest workflow state used to reject stale automatic retry decisions.
#[derive(Debug, Clone, Deserialize)]
pub struct RunState {
    /// GitHub run identity.
    pub id: i64,
    /// Latest run attempt.
    pub run_attempt: u32,
    /// Only `completed` runs can be automatically retried.
    pub status: String,
}
/// Job metadata for a complete, attempt-scoped failure inventory.
#[derive(Debug, Clone, Deserialize)]
pub struct RunJob {
    /// Numeric GitHub job ID.
    pub id: i64,
    /// Execution status.
    pub status: String,
    /// Authoritative workflow job conclusion.
    pub conclusion: Option<String>,
}
struct Token {
    secret: SecretString,
    expires: i64,
}
/// Bounded HTTP client, no redirects or implicit retries. Installation token
/// refresh is single-flight. Only a definitive 401 permits one authenticated replay;
/// ambiguous mutating requests are never resent.
pub struct RestClient {
    api_base: reqwest::Url,
    auth: Auth,
    http: reqwest::Client,
    token: Mutex<Option<Token>>,
}
impl RestClient {
    /// Construct for GitHub.com or an enterprise `/api/v3/` base. HTTP is allowed
    /// only on loopback for local contract tests. Credentials never follow redirects.
    pub fn new(mut api_base: reqwest::Url, auth: Auth) -> Result<Self, Error> {
        let local = matches!(
            api_base.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]")
        );
        if !(api_base.scheme() == "https" || (local && api_base.scheme() == "http"))
            || !api_base.username().is_empty()
            || api_base.password().is_some()
            || api_base.query().is_some()
            || api_base.fragment().is_some()
        {
            return Err(Error::Config);
        }
        if !api_base.path().ends_with('/') {
            api_base.set_path(&format!("{}/", api_base.path()));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .user_agent("runwell/0.1")
            .build()
            .map_err(|_| Error::Config)?;
        Ok(Self {
            api_base,
            auth,
            http,
            token: Mutex::new(None),
        })
    }
    /// Construct using existing App/PAT configuration references.
    pub async fn from_config(
        base: reqwest::Url,
        config: &runwell_config::AuthConfig,
    ) -> Result<Self, Error> {
        Self::new(base, Auth::from_config(config).await?)
    }
    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &SecretString,
    ) -> Result<reqwest::RequestBuilder, Error> {
        if path.starts_with('/')
            || path.contains("://")
            || path.contains("..")
            || path.contains('#')
        {
            return Err(Error::Config);
        }
        let url = self.api_base.join(path).map_err(|_| Error::Config)?;
        if url.origin() != self.api_base.origin() || !url.path().starts_with(self.api_base.path()) {
            return Err(Error::Config);
        }
        Ok(self
            .http
            .request(method, url)
            .bearer_auth(token.expose_secret())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10"))
    }
    /// Get a cached installation token or exchange a fresh, bounded App JWT.
    /// PAT configurations return their PAT, without an exchange.
    pub async fn installation_token(&self) -> Result<SecretString, Error> {
        let Auth::App {
            app_id,
            installation_id,
            private_key,
        } = &self.auth
        else {
            return match &self.auth {
                Auth::Pat(pat) => Ok(pat.clone()),
                _ => Err(Error::Auth),
            };
        };
        if *installation_id == 0 {
            return Err(Error::Config);
        }
        let mut cached = self.token.lock().await;
        let now = jiff::Timestamp::now().as_second();
        if let Some(token) = cached
            .as_ref()
            .filter(|t| t.expires > now.saturating_add(60))
        {
            return Ok(token.secret.clone());
        }
        let jwt = Rs256Signer.sign(
            *app_id,
            private_key,
            u64::try_from(now).map_err(|_| Error::Auth)?,
        )?;
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("app/installations/{installation_id}/access_tokens"),
                &jwt,
            )?
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|_| Error::Transport)?;
        if response.status().as_u16() != 201 {
            return Err(Error::Status(response.status().as_u16()));
        }
        #[derive(Deserialize)]
        struct Exchange {
            token: String,
            expires_at: String,
        }
        let body: Exchange = response.json().await.map_err(|_| Error::Response)?;
        let expires = body
            .expires_at
            .parse::<jiff::Timestamp>()
            .map_err(|_| Error::Response)?
            .as_second();
        if body.token.is_empty() || expires <= now.saturating_add(60) {
            return Err(Error::Response);
        }
        let secret = SecretString::from(body.token);
        *cached = Some(Token {
            secret: secret.clone(),
            expires,
        });
        Ok(secret)
    }
    async fn invalidate_token(&self, rejected: &SecretString) {
        let mut cached = self.token.lock().await;
        // A late 401 must not evict a replacement fetched for another request.
        if cached
            .as_ref()
            .is_some_and(|token| token.secret.expose_secret() == rejected.expose_secret())
        {
            *cached = None;
        }
    }
    async fn authenticated_send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<reqwest::Response, Error> {
        let mut token = self.installation_token().await?;
        for attempt in 0..=1 {
            let mut request = self.request(method.clone(), path, &token)?;
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await.map_err(|_| Error::Transport)?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED
                && matches!(&self.auth, Auth::App { .. })
            {
                self.invalidate_token(&token).await;
                if attempt == 0 {
                    // Authentication was explicitly rejected, so no mutation was
                    // accepted. Refresh under the cache lock and replay only once.
                    drop(response);
                    token = self.installation_token().await?;
                    continue;
                }
            }
            return Ok(response);
        }
        Err(Error::Auth)
    }
    /// Read a relative resource; rejects external URLs and traversal.
    pub async fn get(&self, path: &str) -> Result<serde_json::Value, Error> {
        let response = self
            .authenticated_send(reqwest::Method::GET, path, None)
            .await?;
        if !response.status().is_success() {
            return Err(Error::Status(response.status().as_u16()));
        }
        response.json().await.map_err(|_| Error::Response)
    }
    /// Read the latest run attempt before retrying; no stale-event assumption.
    pub async fn run_state(&self, repo: &str, run: i64) -> Result<RunState, Error> {
        let path = run_path(repo, run)?;
        let state: RunState =
            serde_json::from_value(self.get(&path).await?).map_err(|_| Error::Response)?;
        if state.id != run || state.run_attempt == 0 {
            return Err(Error::Response);
        }
        Ok(state)
    }
    /// Fetch all jobs of one attempt, with a bounded pagination loop. Partial,
    /// changing or duplicate inventories are rejected instead of authorizing retries.
    pub async fn attempt_jobs(
        &self,
        repo: &str,
        run: i64,
        attempt: u32,
    ) -> Result<Vec<RunJob>, Error> {
        if attempt == 0 {
            return Err(Error::Config);
        }
        let path = run_path(repo, run)?;
        let mut jobs = Vec::new();
        let mut ids = std::collections::BTreeSet::new();
        let mut expected = None;
        for page in 1..=100 {
            #[derive(Deserialize)]
            struct Page {
                total_count: usize,
                jobs: Vec<RunJob>,
            }
            let body: Page = serde_json::from_value(
                self.get(&format!(
                    "{path}/attempts/{attempt}/jobs?per_page=100&page={page}"
                ))
                .await?,
            )
            .map_err(|_| Error::Response)?;
            if expected.is_some_and(|n| n != body.total_count)
                || body.jobs.is_empty() && body.total_count > jobs.len()
            {
                return Err(Error::Response);
            }
            expected = Some(body.total_count);
            for job in body.jobs {
                if job.id <= 0 || !ids.insert(job.id) {
                    return Err(Error::Response);
                }
                jobs.push(job);
            }
            if jobs.len() == body.total_count {
                return Ok(jobs);
            }
            if jobs.len() > body.total_count {
                return Err(Error::Response);
            }
        }
        Err(Error::Response)
    }
    /// POST rerun-failed-jobs, replaying once only after an explicit App-token 401.
    /// This reruns ALL failures and dependents; callers must validate the complete
    /// failure set and persist a claim first. Ambiguous requests are never resent.
    pub async fn rerun_failed_jobs(&self, repo: &str, run: i64) -> Result<(), Error> {
        let path = format!("{}/rerun-failed-jobs", run_path(repo, run)?);
        let response = self
            .authenticated_send(
                reqwest::Method::POST,
                &path,
                Some(&serde_json::json!({"enable_debug_logging":false})),
            )
            .await?;
        if response.status().as_u16() == 201 {
            Ok(())
        } else {
            Err(Error::Status(response.status().as_u16()))
        }
    }
}
fn run_path(repo: &str, run: i64) -> Result<String, Error> {
    if run <= 0 || !runwell_config::valid_repository(repo) {
        return Err(Error::Config);
    }
    Ok(format!("repos/{repo}/actions/runs/{run}"))
}
