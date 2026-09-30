use crate::{
    Error,
    auth::Credentials,
    retry::{Clock, RetryConfig, SystemClock},
};
use reqwest::Url;
use std::{sync::Arc, time::Duration};

/// Connection and retry configuration. Construct with [`Config::new`]; the API
/// base override supports GHES and local contract tests without live credentials.
pub struct Config {
    pub(crate) config_url: String,
    pub(crate) registration_path: String,
    pub(crate) credentials: Credentials,
    /// GitHub REST base URL, including `/api/v3` for GHES.
    pub github_api_url: Url,
    /// Transport retry limits.
    pub retry: RetryConfig,
    /// Clock and sleeper, shared by auth, retries and session conflict handling.
    pub clock: Arc<dyn Clock>,
    /// Maximum duration spent retrying 409 session conflicts (default five minutes).
    pub session_conflict_timeout: Duration,
    /// Ordinary request timeout (default 30 seconds).
    pub request_timeout: Duration,
    /// Long-poll request timeout (default two minutes; must exceed 50 seconds).
    pub poll_timeout: Duration,
    /// JSON telemetry User-Agent; contains no credentials.
    pub user_agent: String,
}
impl Config {
    /// Parse a repository, organization or enterprise configuration URL. Enterprise
    /// scope only supports PATs. `GITHUB_ACTIONS_FORCE_GHES` forces `/api/v3` routing.
    pub fn new(config_url: impl Into<String>, credentials: Credentials) -> Result<Self, Error> {
        let config_url = config_url.into();
        let url = parse_url(&config_url)?;
        let parts: Vec<_> = url.path().trim_matches('/').split('/').collect();
        let registration_path = match parts.as_slice() {
            [org] if !org.is_empty() => format!("orgs/{org}"),
            [prefix, enterprise]
                if prefix.eq_ignore_ascii_case("enterprises") && !enterprise.is_empty() =>
            {
                if matches!(credentials, Credentials::App { .. }) {
                    return Err(Error::Config("enterprise scope requires a PAT"));
                }
                format!("enterprises/{enterprise}")
            }
            [owner, repo] if !owner.is_empty() && !repo.is_empty() => {
                format!("repos/{owner}/{repo}")
            }
            _ => return Err(Error::Config("expected an organization or repository URL")),
        } + "/actions/runners/registration-token";
        let host = url.host_str().ok_or(Error::Config("missing GitHub host"))?;
        let hosted = host == "github.com"
            || host == "www.github.com"
            || host == "github.localhost"
            || host.ends_with(".ghe.com");
        let github_api_url = if hosted && std::env::var_os("GITHUB_ACTIONS_FORCE_GHES").is_none() {
            parse_url(&format!("https://api.{}/", host.trim_start_matches("www.")))?
        } else {
            let mut base = url.clone();
            base.set_path("/api/v3/");
            base
        };
        Ok(Self {
            config_url,
            registration_path,
            credentials,
            github_api_url,
            retry: RetryConfig::default(),
            clock: Arc::new(SystemClock),
            session_conflict_timeout: Duration::from_secs(300),
            request_timeout: Duration::from_secs(30),
            poll_timeout: Duration::from_secs(120),
            user_agent: serde_json::json!({"system":"runwell","version":env!("CARGO_PKG_VERSION"),
                "subsystem":"listener","kind":"scaleset","build_version":env!("CARGO_PKG_VERSION")})
            .to_string(),
        })
    }
}

pub(crate) fn parse_url(value: &str) -> Result<Url, Error> {
    let url = Url::parse(value).map_err(|_| Error::Config("invalid URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Config(
            "expected an HTTP(S) URL without userinfo or fragment",
        ));
    }
    Ok(url)
}
