use crate::{Classifier, Error, FailureClass, FailureEvidence, RetryApi, RetryJournal};
use runwell_store::{RetryClaim, RetryStatus};
use std::collections::BTreeMap;

/// Automatic retry is opt-in; a zero daily cap also disables it.
#[derive(Debug, Clone, Default)]
pub struct RetryPolicy {
    /// Operator off switch.
    pub enabled: bool,
    /// Maximum failed jobs retried per repository per UTC day.
    pub daily_cap: u32,
}
/// Request tied to a failed attempt, with evidence indexed by REST job ID.
#[derive(Debug, Clone)]
pub struct RetryRequest {
    /// Canonical repository (case normalized by the retry service).
    pub repo: String,
    /// REST workflow run identity.
    pub run_id: i64,
    /// Failed attempt, starting at one.
    pub attempt: u32,
    /// Evidence collected for this exact run attempt.
    pub evidence: BTreeMap<i64, FailureEvidence>,
}
/// Successful policy evaluation, including reasons for safely doing nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Off switch or zero budget; no API or store calls were made.
    Disabled,
    /// Run is not complete or a newer attempt exists.
    Stale,
    /// Missing evidence, non-infra failure, or no failures.
    Ineligible,
    /// Already claimed, automatic retry chain, or daily cap reached.
    Suppressed,
    /// GitHub accepted one POST and the outcome was persisted.
    Accepted,
}
/// Inspect the full remote failure set, claim durably, then attempt one logical
/// rerun. The API may replay once after a definitive authentication rejection.
/// Ambiguous/crash outcomes retain their claim forever until operator review.
/// GitHub has no conditional attempt/POST API; an external manual rerun racing
/// the final check cannot be atomically excluded by the service.
pub async fn retry(
    policy: &RetryPolicy,
    classifier: &Classifier,
    journal: &dyn RetryJournal,
    api: &dyn RetryApi,
    request: &RetryRequest,
    now_unix: i64,
) -> Result<Outcome, Error> {
    if !policy.enabled || policy.daily_cap == 0 {
        return Ok(Outcome::Disabled);
    }
    if request.run_id <= 0 || request.attempt == 0 {
        return Ok(Outcome::Ineligible);
    }
    let repo = request.repo.to_lowercase();
    let current = api.run_state(&repo, request.run_id).await?;
    if current.id != request.run_id
        || current.run_attempt != request.attempt
        || current.status != "completed"
    {
        return Ok(Outcome::Stale);
    }
    let jobs = api
        .attempt_jobs(&repo, request.run_id, request.attempt)
        .await?;
    let mut failures = Vec::new();
    for job in jobs {
        if job.status != "completed" {
            return Ok(Outcome::Ineligible);
        }
        if matches!(
            job.conclusion.as_deref(),
            Some("success" | "skipped" | "neutral")
        ) {
            continue;
        }
        let Some(evidence) = request.evidence.get(&job.id) else {
            return Ok(Outcome::Ineligible);
        };
        if Some(evidence.conclusion.as_str()) != job.conclusion.as_deref()
            || classifier.classify(evidence).class != FailureClass::Infra
        {
            return Ok(Outcome::Ineligible);
        }
        failures.push(job.id);
    }
    if failures.is_empty() {
        return Ok(Outcome::Ineligible);
    }
    let current = api.run_state(&repo, request.run_id).await?;
    if current.run_attempt != request.attempt || current.status != "completed" {
        return Ok(Outcome::Stale);
    }
    if !journal
        .claim(RetryClaim {
            repo: repo.clone(),
            run_id: request.run_id,
            attempt: request.attempt,
            job_ids: failures,
            utc_day: now_unix.div_euclid(86_400),
            daily_cap: policy.daily_cap,
        })
        .await?
    {
        return Ok(Outcome::Suppressed);
    }
    let result = api.rerun_failed_jobs(&repo, request.run_id).await;
    let status = match &result {
        Ok(()) => RetryStatus::Accepted,
        Err(Error::Github(e)) if !e.ambiguous() => RetryStatus::Rejected,
        Err(_) => RetryStatus::Ambiguous,
    };
    journal
        .finish(&repo, request.run_id, request.attempt, status)
        .await?;
    result?;
    Ok(Outcome::Accepted)
}
