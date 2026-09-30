//! Paginated GitHub collection with bounded retries and server-directed backoff.
use crate::{
    Error,
    cache::Cache,
    trace_build::{self, Run},
};
use base64::Engine;
use jiff::Timestamp;
use runwell_trace::TraceJob;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::Duration,
};

pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
    cache: Cache,
    blocked_until: Option<tokio::time::Instant>,
}

pub async fn token() -> Result<String, Error> {
    for name in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(name)
            && !value.trim().is_empty()
        {
            return Ok(value);
        }
    }
    let output = tokio::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .await
        .map_err(|_| {
            Error::Invalid("set GH_TOKEN/GITHUB_TOKEN or authenticate with gh auth login".into())
        })?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || value.is_empty() {
        return Err(Error::Invalid(
            "GitHub authentication unavailable; set GH_TOKEN/GITHUB_TOKEN or use gh auth login"
                .into(),
        ));
    }
    Ok(value)
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
            blocked_until: None,
        })
    }

    pub async fn raw(&mut self, path: &str, immutable: bool) -> Result<String, Error> {
        let key = format!("{}{}", self.base, path);
        if let Some(body) = self.cache.get(&key, immutable)? {
            return Ok(body);
        }
        for attempt in 0..6u32 {
            if let Some(until) = self.blocked_until.take() {
                tokio::time::sleep_until(until).await;
            }
            let response = self
                .http
                .get(&key)
                .bearer_auth(&self.token)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2026-03-10")
                .send()
                .await?;
            let status = response.status();
            let headers = response.headers();
            let remaining = headers
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok());
            let retry = headers
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            let reset = headers
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<i64>().ok());
            let reset_delay =
                reset.map(|r| r.saturating_sub(Timestamp::now().as_second()).max(1) as u64);
            let exhausted = remaining == Some("0");
            let body = response.text().await?;
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
                return Err(Error::Http(status.as_u16()));
            }
            self.cache.put(&key, body.clone())?;
            return Ok(body);
        }
        Err(Error::Invalid("rate-limit retry budget exhausted".into()))
    }

    pub async fn json(&mut self, path: &str, immutable: bool) -> Result<Value, Error> {
        Ok(serde_json::from_str(&self.raw(path, immutable).await?)?)
    }

    pub async fn pages(&mut self, path: &str, field: Option<&str>) -> Result<Vec<Value>, Error> {
        let separator = if path.contains('?') { '&' } else { '?' };
        let mut values = Vec::new();
        for page in 1..=100_000 {
            let response = self
                .json(&format!("{path}{separator}per_page=100&page={page}"), false)
                .await?;
            let array = field
                .and_then(|f| response.get(f))
                .unwrap_or(&response)
                .as_array()
                .ok_or_else(|| Error::Invalid("GitHub returned a non-array page".into()))?;
            values.extend(array.iter().cloned());
            if array.len() < 100 {
                return Ok(values);
            }
        }
        Err(Error::Invalid("pagination limit exceeded".into()))
    }

    // GitHub caps filtered run searches at 1,000. Split crowded windows recursively.
    async fn runs(
        &mut self,
        repo: &str,
        start: Timestamp,
        end: Timestamp,
    ) -> Result<Vec<Run>, Error> {
        let mut windows = vec![(start.as_second(), end.as_second())];
        let mut runs = BTreeMap::new();
        while let Some((s, e)) = windows.pop() {
            let created = format!(
                "{}..{}",
                Timestamp::from_second(s).map_err(|e| Error::Invalid(e.to_string()))?,
                Timestamp::from_second(e).map_err(|e| Error::Invalid(e.to_string()))?
            );
            let created: String =
                reqwest::Url::parse_with_params("https://api.github.com/", [("created", &created)])
                    .map_err(|e| Error::Invalid(e.to_string()))?
                    .query()
                    .unwrap_or_default()
                    .into();
            let path = format!("/repos/{repo}/actions/runs?{created}");
            let first = self
                .json(&format!("{path}&per_page=100&page=1"), false)
                .await?;
            let total = first
                .get("total_count")
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::Invalid("GitHub run page missing total_count".into()))?;
            if total > 1000 {
                if e == s {
                    return Err(Error::Invalid(
                        "more than 1,000 runs in one second; narrow the repository scope".into(),
                    ));
                }
                let middle = s + (e - s) / 2;
                windows.extend([(s, middle), (middle + 1, e)]);
                continue;
            }
            for value in self.pages(&path, Some("workflow_runs")).await? {
                let run: Run = serde_json::from_value(value)?;
                if run.created_at >= start && run.created_at < end {
                    runs.insert(run.id, run);
                }
            }
        }
        Ok(runs.into_values().collect())
    }
}

pub struct Collected {
    pub jobs: Vec<TraceJob>,
    pub warnings: Vec<String>,
}

pub async fn collect(
    client: &mut Client,
    repos: &[String],
    start: Timestamp,
    end: Timestamp,
    logs: usize,
) -> Result<Collected, Error> {
    let mut jobs = Vec::new();
    let mut warnings = Vec::new();
    let mut remaining_logs = logs;
    let mut seen = BTreeSet::new();
    for repo in repos {
        eprintln!("Fetching workflow history for {repo}…");
        for latest in client.runs(repo, start, end).await? {
            // Attempt-specific run metadata preserves the conclusion at that attempt.
            for attempt in 1..=latest.run_attempt {
                let run = if latest.run_attempt == 1 {
                    &latest
                } else {
                    // This binding's lifetime covers the remainder of the loop body.
                    // Deserializing inside the helper avoids mixing latest and first attempts.
                    &serde_json::from_value::<Run>(
                        client
                            .json(
                                &format!(
                                    "/repos/{repo}/actions/runs/{}/attempts/{attempt}",
                                    latest.id
                                ),
                                false,
                            )
                            .await?,
                    )?
                };
                let path = format!(
                    "/repos/{repo}/actions/runs/{}/attempts/{attempt}/jobs",
                    run.id
                );
                let values = client.pages(&path, Some("jobs")).await?;
                let mut batch = Vec::new();
                for value in values {
                    let check_id = value
                        .get("check_run_url")
                        .and_then(Value::as_str)
                        .and_then(|s| s.rsplit('/').next())
                        .and_then(|s| s.parse::<u64>().ok());
                    let mut job = trace_build::job(repo, run, value)?;
                    job.run_attempt = attempt;
                    if !seen.insert((repo.clone(), job.job_id)) {
                        continue;
                    }
                    if matches!(
                        job.conclusion.as_deref(),
                        Some("failure" | "cancelled" | "timed_out" | "startup_failure")
                    ) && crate::metrics::executed(&job)
                    {
                        if let Some(id) = check_id {
                            match client
                                .pages(&format!("/repos/{repo}/check-runs/{id}/annotations"), None)
                                .await
                            {
                                Ok(rows) => {
                                    job.annotations = rows
                                        .iter()
                                        .filter_map(|v| {
                                            v.get("message")
                                                .and_then(Value::as_str)
                                                .map(str::to_owned)
                                        })
                                        .collect()
                                }
                                Err(e) => warnings.push(format!(
                                    "Annotations unavailable for job {}: {e}",
                                    job.job_id.unwrap_or(0)
                                )),
                            }
                        }
                        if remaining_logs > 0 && job.started_at.is_some() {
                            remaining_logs -= 1;
                            if let Some(id) = job.job_id {
                                match client
                                    .raw(&format!("/repos/{repo}/actions/jobs/{id}/logs"), false)
                                    .await
                                {
                                    Ok(text) => job.log_excerpt = Some(text),
                                    Err(e) => {
                                        warnings.push(format!("Logs unavailable for job {id}: {e}"))
                                    }
                                }
                            }
                        }
                    }
                    batch.push(job);
                }
                if let (Some(path), Some(sha)) = (&run.path, &run.head_sha) {
                    let path = path.split('@').next().unwrap_or(path);
                    let url = format!("/repos/{repo}/contents/{path}?ref={sha}");
                    match workflow(client, &url).await {
                        Ok(source) => match trace_build::apply_workflow(&mut batch, &source) {
                            Ok(true) => {}
                            Ok(false) => warnings.push(
                                "Ambiguous workflow graph; using timestamp inference.".into(),
                            ),
                            Err(e) => warnings.push(format!(
                                "Workflow YAML could not be parsed; using timestamp inference: {e}"
                            )),
                        },
                        Err(e) => warnings.push(format!(
                            "Workflow YAML unavailable; using timestamp inference: {e}"
                        )),
                    }
                }
                jobs.extend(batch);
            }
        }
    }
    warnings.sort();
    warnings.dedup();
    Ok(Collected { jobs, warnings })
}

async fn workflow(client: &mut Client, path: &str) -> Result<String, Error> {
    let value = client.json(path, true).await?;
    let content = value
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Invalid("workflow content missing".into()))?;
    let content: String = content.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    String::from_utf8(bytes).map_err(|e| Error::Invalid(e.to_string()))
}
