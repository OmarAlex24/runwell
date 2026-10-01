use crate::Error;
use runwell_github::{RestClient, RunJob, RunState};
use runwell_store::{RetryClaim, RetryStatus, Store};
use std::{future::Future, pin::Pin};
/// Object-safe async operation for controller adapters.
pub type RetryFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;
/// GitHub port; implementations must never replay ambiguous mutations. One
/// authenticated replay after a definitive HTTP 401 rejection is permitted.
pub trait RetryApi: Send + Sync {
    /// Latest run, independent of potentially stale queue events.
    fn run_state<'a>(&'a self, repo: &'a str, run: i64) -> RetryFuture<'a, RunState>;
    /// Complete attempt inventory, including successes/skips and every failure.
    fn attempt_jobs<'a>(
        &'a self,
        repo: &'a str,
        run: i64,
        attempt: u32,
    ) -> RetryFuture<'a, Vec<RunJob>>;
    /// One logical rerun operation after a successful durable claim.
    fn rerun_failed_jobs<'a>(&'a self, repo: &'a str, run: i64) -> RetryFuture<'a, ()>;
}
impl RetryApi for RestClient {
    fn run_state<'a>(&'a self, repo: &'a str, run: i64) -> RetryFuture<'a, RunState> {
        Box::pin(async move { Ok(self.run_state(repo, run).await?) })
    }
    fn attempt_jobs<'a>(
        &'a self,
        repo: &'a str,
        run: i64,
        attempt: u32,
    ) -> RetryFuture<'a, Vec<RunJob>> {
        Box::pin(async move { Ok(self.attempt_jobs(repo, run, attempt).await?) })
    }
    fn rerun_failed_jobs<'a>(&'a self, repo: &'a str, run: i64) -> RetryFuture<'a, ()> {
        Box::pin(async move { Ok(self.rerun_failed_jobs(repo, run).await?) })
    }
}
/// Durable at-most-once claim and repository daily budget boundary.
pub trait RetryJournal: Send + Sync {
    /// Atomic claim; false means duplicate, retry chain, or exhausted budget.
    fn claim(&self, claim: RetryClaim) -> RetryFuture<'_, bool>;
    /// Record outcome without releasing the claim or its budget.
    fn finish<'a>(
        &'a self,
        repo: &'a str,
        run: i64,
        attempt: u32,
        status: RetryStatus,
    ) -> RetryFuture<'a, ()>;
}
impl RetryJournal for Store {
    fn claim(&self, claim: RetryClaim) -> RetryFuture<'_, bool> {
        Box::pin(async move { Ok(self.claim_retry(claim).await?) })
    }
    fn finish<'a>(
        &'a self,
        repo: &'a str,
        run: i64,
        attempt: u32,
        status: RetryStatus,
    ) -> RetryFuture<'a, ()> {
        Box::pin(async move { Ok(self.finish_retry(repo, run, attempt, status).await?) })
    }
}
