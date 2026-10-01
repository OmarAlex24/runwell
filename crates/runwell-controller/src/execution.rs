//! Resolve scale-set UUIDs through the actual unique runner identity, never names alone.
use runwell_github::RestClient;
use runwell_node::{Error, NodeFuture};

/// Authoritative REST execution for one controller-owned runner.
#[derive(Clone)]
pub struct ExecutionResult {
    /// Numeric REST job identity, distinct from the scale-set UUID.
    pub job_id: i64,
    /// Actual REST run attempt.
    pub attempt: u32,
    /// Authoritative job conclusion.
    pub conclusion: String,
    /// A completed failed workflow step vetoes automatic retries conservatively.
    pub code_failure: bool,
    /// Available bounded check annotations; never logged or persisted.
    pub annotations: String,
}
/// Read-only execution lookup, independently fakeable from retry mutations.
pub trait ExecutionSource: Send + Sync {
    /// None means remote completion/identity is not yet available; retain the observation.
    fn execution<'a>(
        &'a self,
        repo: &'a str,
        run: i64,
        runner: &'a str,
    ) -> NodeFuture<'a, Option<ExecutionResult>>;
}
impl ExecutionSource for RestClient {
    fn execution<'a>(
        &'a self,
        repo: &'a str,
        run: i64,
        runner: &'a str,
    ) -> NodeFuture<'a, Option<ExecutionResult>> {
        Box::pin(async move {
            if !runwell_config::valid_repository(repo) || run <= 0 || runner.is_empty() {
                return Ok(None);
            }
            let state = self.run_state(repo, run).await.map_err(|_| Error::Github)?;
            if state.status != "completed" {
                return Ok(None);
            }
            let mut matching = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            let mut count = None;
            for page in 1..=100 {
                let body = self
                    .get(&format!(
                        "repos/{repo}/actions/runs/{run}/attempts/{}/jobs?per_page=100&page={page}",
                        state.run_attempt
                    ))
                    .await
                    .map_err(|_| Error::Github)?;
                let total = body["total_count"].as_u64().ok_or(Error::Github)? as usize;
                let jobs = body["jobs"].as_array().ok_or(Error::Github)?;
                if count.is_some_and(|n| n != total) || jobs.is_empty() && seen.len() < total {
                    return Err(Error::Github);
                }
                count = Some(total);
                for job in jobs {
                    let id = job["id"]
                        .as_i64()
                        .filter(|id| *id > 0)
                        .ok_or(Error::Github)?;
                    if !seen.insert(id) {
                        return Err(Error::Github);
                    }
                    if job["runner_name"].as_str() == Some(runner) {
                        matching.push(job.clone());
                    }
                }
                if seen.len() > total {
                    return Err(Error::Github);
                }
                if seen.len() == total {
                    break;
                }
            }
            if count != Some(seen.len()) {
                return Err(Error::Github);
            }
            if matching.len() != 1 {
                return Ok(None);
            }
            let job = &matching[0];
            if job["status"].as_str() != Some("completed") {
                return Ok(None);
            }
            let Some(conclusion) = job["conclusion"].as_str() else {
                return Ok(None);
            };
            // A failed step may itself be infrastructure-related. Without complete
            // log evidence we deliberately veto it instead of risking a red-test retry.
            let code_failure = job["steps"].as_array().is_none_or(|steps| {
                steps
                    .iter()
                    .any(|s| s["conclusion"].as_str() == Some("failure"))
            });
            Ok(Some(ExecutionResult {
                job_id: job["id"].as_i64().ok_or(Error::Github)?,
                attempt: state.run_attempt,
                conclusion: conclusion.into(),
                code_failure,
                annotations: String::new(),
            }))
        })
    }
}
