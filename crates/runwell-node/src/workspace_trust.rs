use crate::Error;
use runwell_config::{AuthConfig, GithubConfig};
use runwell_store::Job;
use runwell_workspace::Completion;
use serde::{Deserialize, Serialize};

/// Promotion evidence comes from authenticated REST, not a job-writable file or
/// job_workflow_ref (a reusable workflow's ref can differ from the run head).
pub(crate) struct WorkspaceTrust {
    client: reqwest::Client,
    api: reqwest::Url,
    auth: AuthConfig,
}
#[derive(Deserialize)]
struct Repository {
    full_name: String,
    #[serde(default)]
    default_branch: String,
}
#[derive(Deserialize)]
struct Run {
    id: i64,
    event: String,
    head_branch: Option<String>,
    repository: Repository,
    head_repository: Option<Repository>,
}
impl WorkspaceTrust {
    pub fn new(config: &GithubConfig) -> Result<Self, Error> {
        let mut api = reqwest::Url::parse(&config.config_url).map_err(|_| Error::Config)?;
        if api.host_str() == Some("github.com") {
            api = reqwest::Url::parse("https://api.github.com/").map_err(|_| Error::Config)?;
        } else {
            api.set_path("/api/v3/");
        }
        api.set_query(None);
        api.set_fragment(None);
        let client = reqwest::Client::builder()
            .user_agent("runwell-workspace")
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::Github)?;
        Ok(Self {
            client,
            api,
            auth: config.auth.clone(),
        })
    }
    pub async fn completion(&self, job: &Job) -> Result<Completion, Error> {
        let repo = &job.metadata.repo;
        if !runwell_config::valid_repository(repo) || job.metadata.workflow_run_id <= 0 {
            return Err(Error::Config);
        }
        let token = self.token().await?;
        let run: Run = self
            .get(
                &format!("repos/{repo}/actions/runs/{}", job.metadata.workflow_run_id),
                &token,
            )
            .await?;
        let repository: Repository = self.get(&format!("repos/{repo}"), &token).await?;
        if run.id != job.metadata.workflow_run_id
            || !run.repository.full_name.eq_ignore_ascii_case(repo)
            || !repository.full_name.eq_ignore_ascii_case(repo)
        {
            return Err(Error::Github);
        }
        Ok(Completion {
            repository: repo.clone(),
            succeeded: job.state == runwell_store::State::Completed
                && job
                    .outcome
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case("succeeded")),
            same_repository: run
                .head_repository
                .is_some_and(|r| r.full_name.eq_ignore_ascii_case(repo)),
            event: run.event,
            branch: run.head_branch.unwrap_or_default(),
            default_branch: repository.default_branch,
        })
    }
    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        token: &str,
    ) -> Result<T, Error> {
        self.client
            .get(self.api.join(path).map_err(|_| Error::Config)?)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|_| Error::Github)?
            .error_for_status()
            .map_err(|_| Error::Github)?
            .json()
            .await
            .map_err(|_| Error::Github)
    }
    async fn token(&self) -> Result<String, Error> {
        let (app, installation, key) = match &self.auth {
            AuthConfig::Pat { token_file } => {
                return read_secret(std::fs::read_to_string(token_file));
            }
            AuthConfig::PatEnv { token_env } => return read_secret(std::env::var(token_env)),
            AuthConfig::App {
                app_id,
                installation_id,
                private_key_file,
            } => (
                *app_id,
                *installation_id,
                read_secret(std::fs::read_to_string(private_key_file))?,
            ),
            AuthConfig::AppEnv {
                app_id,
                installation_id,
                private_key_env,
            } => (
                *app_id,
                *installation_id,
                read_secret(std::env::var(private_key_env))?,
            ),
        };
        #[derive(Serialize)]
        struct Claims {
            iss: String,
            iat: u64,
            exp: u64,
        }
        #[derive(Deserialize)]
        struct Token {
            token: String,
        }
        let now = jiff::Timestamp::now().as_second().max(0) as u64;
        let key =
            jsonwebtoken::EncodingKey::from_rsa_pem(key.as_bytes()).map_err(|_| Error::Config)?;
        let jwt = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &Claims {
                iss: app.to_string(),
                iat: now.saturating_sub(60),
                exp: now + 480,
            },
            &key,
        )
        .map_err(|_| Error::Config)?;
        let token: Token = self
            .client
            .post(
                self.api
                    .join(&format!("app/installations/{installation}/access_tokens"))
                    .map_err(|_| Error::Config)?,
            )
            .bearer_auth(jwt)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|_| Error::Github)?
            .error_for_status()
            .map_err(|_| Error::Github)?
            .json()
            .await
            .map_err(|_| Error::Github)?;
        read_secret(Ok::<_, Error>(token.token))
    }
}
fn read_secret(value: Result<String, impl std::fmt::Debug>) -> Result<String, Error> {
    let value = value.map_err(|_| Error::Config)?;
    if value.trim().is_empty() {
        return Err(Error::Config);
    }
    Ok(value.trim().into())
}

#[cfg(test)]
#[path = "workspace_trust_tests.rs"]
mod tests;
