use crate::{
    Error, Secret,
    config::Config,
    retry::{Clock, Policy, RetryConfig},
};
use reqwest::{
    Method, StatusCode, Url,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::de::DeserializeOwned;
use std::{sync::Arc, time::Duration};

pub(crate) struct Transport {
    http: reqwest::Client,
    pub clock: Arc<dyn Clock>,
    pub retry: RetryConfig,
    pub timeout: Duration,
    pub poll_timeout: Duration,
    user_agent: String,
}

pub(crate) struct Request {
    pub method: Method,
    pub url: Url,
    pub body: Option<serde_json::Value>,
    pub policy: Policy,
    pub content_type: &'static str,
    pub capacity: Option<u32>,
}
impl Request {
    pub fn new(method: Method, url: Url, policy: Policy) -> Self {
        Self {
            method,
            url,
            body: None,
            policy,
            content_type: "application/json",
            capacity: None,
        }
    }
}

pub(crate) struct Response {
    pub status: StatusCode,
    pub endpoint: String,
    headers: HeaderMap,
    body: Vec<u8>,
}
impl Response {
    pub fn require(self, statuses: &[StatusCode]) -> Result<Self, Error> {
        if statuses.contains(&self.status) {
            Ok(self)
        } else {
            Err(self.error())
        }
    }
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, Error> {
        serde_json::from_slice(strip_bom(&self.body))
            .map_err(|_| Error::protocol(&self.endpoint, "invalid JSON payload"))
    }
    pub fn error(self) -> Error {
        let parsed: serde_json::Value =
            serde_json::from_slice(strip_bom(&self.body)).unwrap_or_default();
        // Keep exception classification, never arbitrary server messages or echoed credentials.
        let error_type = parsed.get("typeName").and_then(|v| v.as_str()).map(|s| {
            s.split(',')
                .next()
                .unwrap_or_default()
                .chars()
                .take(256)
                .collect()
        });
        Error::Http {
            status: self.status,
            endpoint: self.endpoint,
            error_type,
            activity_id: header(&self.headers, "activityid"),
            request_id: header(&self.headers, "x-github-request-id"),
        }
    }
}
fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}
pub(crate) fn strip_bom(body: &[u8]) -> &[u8] {
    body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body)
}
pub(crate) fn join(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    url.set_path(&format!(
        "{}/{}",
        base.path().trim_end_matches('/'),
        path.trim_start_matches('/')
    ));
    url
}
pub(crate) fn query(url: &mut Url, key: &str, value: &str) {
    let pairs: Vec<_> = url
        .query_pairs()
        .filter(|(k, _)| k != key)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(pairs)
        .append_pair(key, value);
}
impl Transport {
    pub fn new(config: &Config) -> Result<Self, Error> {
        // Refuse redirects: no implicit replay of mutations or cross-host credential forwarding.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(config.request_timeout)
            .build()
            .map_err(|e| Error::Transport {
                endpoint: "HTTP client".into(),
                source: e.without_url(),
            })?;
        Ok(Self {
            http,
            clock: config.clock.clone(),
            retry: config.retry.clone(),
            timeout: config.request_timeout,
            poll_timeout: config.poll_timeout,
            user_agent: config.user_agent.clone(),
        })
    }

    #[tracing::instrument(skip_all, fields(method = %request.method, endpoint = %request.url.path()))]
    pub async fn send(
        &self,
        request: &Request,
        scheme: &str,
        token: &Secret,
    ) -> Result<Response, Error> {
        let mut auth = HeaderValue::from_str(&format!("{scheme} {}", token.expose()))
            .map_err(|_| Error::Config("credential is not a valid HTTP header"))?;
        auth.set_sensitive(true);
        let mut url = request.url.clone();
        query(&mut url, "api-version", "6.0-preview");
        let mut attempt = 0;
        loop {
            let mut builder = self
                .http
                .request(request.method.clone(), url.clone())
                .header(AUTHORIZATION, auth.clone())
                .header("User-Agent", &self.user_agent)
                .header("Content-Type", request.content_type)
                .header("Accept", "application/json; api-version=6.0-preview")
                .timeout(if request.capacity.is_some() {
                    self.poll_timeout
                } else {
                    self.timeout
                });
            if let Some(capacity) = request.capacity {
                builder = builder.header("X-ScaleSetMaxCapacity", capacity);
            }
            if let Some(body) = &request.body {
                builder = builder.json(body);
            }
            let result = self.execute(builder, url.path()).await;
            let retry = attempt < self.retry.max_retries
                && match &result {
                    Ok(response) => request.policy.retries(response.status),
                    Err(Error::Transport { source, .. }) => {
                        matches!(request.policy, Policy::Idempotent)
                            && (source.is_timeout() || source.is_connect() || source.is_body())
                            && !source.is_builder()
                    }
                    _ => false,
                };
            if !retry {
                return result;
            }
            let delay = match &result {
                Ok(response) if matches!(response.status.as_u16(), 429 | 503) => self
                    .retry_after(&response.headers)
                    .unwrap_or_else(|| self.retry.delay(attempt)),
                _ => self.retry.delay(attempt),
            };
            tracing::debug!(attempt, ?delay, "retrying eligible request");
            self.clock.sleep(delay).await;
            attempt += 1;
        }
    }

    async fn execute(
        &self,
        builder: reqwest::RequestBuilder,
        endpoint: &str,
    ) -> Result<Response, Error> {
        let response = builder.send().await.map_err(|e| Error::Transport {
            endpoint: endpoint.into(),
            source: e.without_url(),
        })?;
        let status = response.status();
        let headers = response.headers().clone();
        tracing::debug!(%status, activity_id = ?header(&headers, "activityid"),
            request_id = ?header(&headers, "x-github-request-id"), "HTTP response");
        // Preserve the status even if an error response body is interrupted.
        let body = match response.bytes().await {
            Ok(body) => body.to_vec(),
            Err(_) if !status.is_success() => Vec::new(),
            Err(e) => {
                return Err(Error::Transport {
                    endpoint: endpoint.into(),
                    source: e.without_url(),
                });
            }
        };
        Ok(Response {
            status,
            endpoint: endpoint.into(),
            headers,
            body,
        })
    }

    fn retry_after(&self, headers: &HeaderMap) -> Option<Duration> {
        let value = headers.get("retry-after")?.to_str().ok()?;
        let delay = value
            .parse::<u64>()
            .map(Duration::from_secs)
            .ok()
            .or_else(|| {
                httpdate::parse_http_date(value)
                    .ok()
                    .map(|date| date.duration_since(self.clock.now()).unwrap_or_default())
            })?;
        Some(delay.min(self.retry.max_retry_after))
    }
}
