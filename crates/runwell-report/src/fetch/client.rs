//! Cached HTTP requests with bounded rate-limit and transient retries.
use crate::{Error, cache::Cache};
use jiff::Timestamp;
use std::{
    error::Error as _,
    future::Future,
    io::ErrorKind,
    path::PathBuf,
    time::{Duration, SystemTime},
};
use tokio::time::Instant;

pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
    cache: Cache,
    cached_only: bool,
    blocked_until: Option<tokio::time::Instant>,
}

impl Client {
    pub fn new(base: String, token: String, cache_dir: PathBuf) -> Result<Self, Error> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent("runwell-report/0.1")
                .timeout(Duration::from_secs(120))
                .build()?,
            base: base.trim_end_matches('/').into(),
            token,
            cache: Cache::new(cache_dir)?,
            cached_only: false,
            blocked_until: None,
        })
    }

    /// Read the existing snapshot without authentication or network fallback.
    pub fn offline_cache(&mut self) {
        self.cached_only = true;
    }

    pub async fn raw(&mut self, path: &str, immutable: bool) -> Result<String, Error> {
        self.raw_with_sleep(path, immutable, tokio::time::sleep_until)
            .await
    }

    async fn raw_with_sleep<F, Fut>(
        &mut self,
        path: &str,
        immutable: bool,
        mut sleep: F,
    ) -> Result<String, Error>
    where
        F: FnMut(Instant) -> Fut,
        Fut: Future<Output = ()>,
    {
        let key = format!("{}{}", self.base, path);
        if let Some(body) = self.cache.get(&key, immutable || self.cached_only)? {
            return Ok(body);
        }
        if self.cached_only {
            return Err(Error::Invalid("response missing from offline cache".into()));
        }
        for attempt in 0..6u32 {
            if let Some(until) = self.blocked_until.take() {
                sleep(until).await;
            }
            let response = self
                .http
                .get(&key)
                .bearer_auth(&self.token)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2026-03-10")
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    retry_transient(error.into(), attempt, None, &mut sleep).await?;
                    continue;
                }
            };
            let status = response.status();
            let headers = response.headers();
            let remaining = headers
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok());
            let retry = headers
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            let transient_retry = retry_after(headers);
            let reset = headers
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<i64>().ok());
            let reset_delay =
                reset.map(|r| r.saturating_sub(Timestamp::now().as_second()).max(1) as u64);
            let exhausted = remaining == Some("0");
            let body = match response.text().await {
                Ok(body) => body,
                Err(error) if status.is_success() => {
                    retry_transient(error.into(), attempt, transient_retry, &mut sleep).await?;
                    continue;
                }
                // Preserve HTTP failures even when their error body cannot be read.
                Err(_) => String::new(),
            };
            let throttled = status.as_u16() == 429
                || (status.as_u16() == 403
                    && (retry.is_some()
                        || exhausted
                        || body.to_ascii_lowercase().contains("rate limit")));
            let delay = retry
                .or_else(|| exhausted.then_some(reset_delay.unwrap_or(60)))
                .unwrap_or(60 * (1u64 << attempt));
            if throttled || exhausted {
                self.blocked_until = Some(tokio::time::Instant::now() + Duration::from_secs(delay));
            }
            if throttled && attempt < 5 {
                eprintln!(
                    "GitHub rate limit: retrying in {delay} seconds (attempt {}).",
                    attempt + 1
                );
                continue;
            }
            if !status.is_success() {
                retry_transient(
                    Error::Http(status.as_u16()),
                    attempt,
                    transient_retry,
                    &mut sleep,
                )
                .await?;
                continue;
            }
            self.cache.put(&key, body.clone())?;
            return Ok(body);
        }
        Err(Error::Invalid("GitHub retry budget exhausted".into()))
    }
}

async fn retry_transient<F, Fut>(
    error: Error,
    attempt: u32,
    retry_after: Option<Duration>,
    sleep: &mut F,
) -> Result<(), Error>
where
    F: FnMut(Instant) -> Fut,
    Fut: Future<Output = ()>,
{
    let kind = match &error {
        Error::Http(status @ (500 | 502 | 503 | 504)) => format!("HTTP {status}"),
        Error::Request(error) if error.is_timeout() => "timeout".into(),
        Error::Request(error) if error.is_connect() => "connection error".into(),
        Error::Request(error) if error.is_body() => "body read error".into(),
        Error::Request(error) if error.is_decode() => "body decode error".into(),
        Error::Request(error) if connection_interrupted(error) => "connection interrupted".into(),
        _ => return Err(error),
    };
    if attempt >= 5 {
        return Err(error);
    }
    let delay = retry_after.unwrap_or_else(|| backoff(attempt));
    eprintln!(
        "GitHub {kind}: retrying in {:.1} seconds (attempt {}).",
        delay.as_secs_f64(),
        attempt + 1
    );
    sleep(Instant::now() + delay).await;
    Ok(())
}

fn connection_interrupted(error: &reqwest::Error) -> bool {
    let mut source = error.source();
    while let Some(error) = source {
        if let Some(error) = error.downcast_ref::<std::io::Error>()
            && matches!(
                error.kind(),
                ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::BrokenPipe
                    | ErrorKind::UnexpectedEof
            )
        {
            return true;
        }
        source = error.source();
    }
    false
}

fn backoff(attempt: u32) -> Duration {
    let ceiling_ms = (2_000 * (1u64 << attempt.min(4))).min(30_000);
    Duration::from_millis(fastrand::u64(ceiling_ms * 3 / 4..=ceiling_ms))
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get("retry-after")?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    httpdate::parse_http_date(value)
        .ok()
        .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
}

#[cfg(test)]
mod tests;
