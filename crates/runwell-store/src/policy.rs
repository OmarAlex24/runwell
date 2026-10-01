//! Durable production scheduling and controller observation state.
use crate::{Error, Placement, Store, writer::Mutation};
use runwell_scheduler::{Criticality, FairState};
use serde::{Deserialize, Serialize};

/// Enriched scheduling metadata; absent graph/PR metadata has explicit fallbacks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchedulingContext {
    /// Stable workflow job key, or namespaced workflow-reference/display-name fallback.
    pub workflow_job: String,
    /// PR identity; empty uses a separate bucket per workflow run.
    pub pull_request: String,
    /// Known DAG shape; absent uses learned history.
    pub criticality: Option<Criticality>,
    /// Controller-clock ready timestamp, in milliseconds.
    pub ready_at_ms: i64,
}
/// Durable input to asynchronous classification, history and alert processing.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Observation {
    /// Local execution identity.
    pub job_id: i64,
    /// Infrastructure/watchdog evidence, if present.
    pub reason: Option<String>,
    /// Final local measurement is available.
    pub completed: bool,
    /// Authoritative result has been processed.
    pub processed: bool,
    /// Ephemeral metrics have already been accounted for this execution.
    pub counted: bool,
    /// Confirmed infrastructure classification.
    pub infra: bool,
    /// Controller-clock event seconds.
    pub observed_at: i64,
}
pub(crate) enum PolicyMutation {
    Context(i64, SchedulingContext),
    Propose(Placement, FairState),
    Accept(i64),
    Reject(i64),
    Abandon(i64),
    Count(i64),
    Observe(i64, Option<String>, bool, i64),
    Processed(i64, bool, bool),
    Expiry(String, i64),
}
impl Store {
    /// Persist metadata supplied by queue events or an optional DAG/PR enricher.
    pub async fn scheduling_context(&self, id: i64) -> Result<Option<SchedulingContext>, Error> {
        let value: Option<String> =
            sqlx::query_scalar("SELECT payload FROM scheduling_context WHERE job_id=?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        value
            .map(|v| serde_json::from_str(&v).map_err(|_| Error::Corrupt))
            .transpose()
    }
    /// Replace metadata when the actual execution differs from acquired demand.
    pub async fn set_scheduling_context(
        &self,
        id: i64,
        context: SchedulingContext,
    ) -> Result<(), Error> {
        self.policy_write(PolicyMutation::Context(id, context))
            .await
            .map(|_| ())
    }
    /// Durable original queue time for events without a queue timestamp.
    pub async fn queued_at(&self, id: i64) -> Result<i64, Error> {
        Ok(
            sqlx::query_scalar("SELECT created_at*1000 FROM jobs WHERE id=?")
                .bind(id)
                .fetch_one(&self.pool)
                .await?,
        )
    }
    /// Last fairness state charged to an accepted reservation.
    pub async fn fairness(&self) -> Result<FairState, Error> {
        let json: String =
            sqlx::query_scalar("SELECT payload FROM scheduler_fairness WHERE singleton=1")
                .fetch_one(&self.pool)
                .await?;
        serde_json::from_str(&json).map_err(|_| Error::Corrupt)
    }
    /// Journal proposed accounting and placement together, before node admission.
    pub async fn propose_dispatch(
        &self,
        placement: Placement,
        next: FairState,
    ) -> Result<(), Error> {
        self.policy_write(PolicyMutation::Propose(placement, next))
            .await
            .map(|_| ())
    }
    /// Charge the proposed accounting exactly once, after node acceptance.
    pub async fn accept_dispatch(&self, id: i64) -> Result<bool, Error> {
        Ok(self.policy_write(PolicyMutation::Accept(id)).await? == 1)
    }
    /// Release only a definitively rejected, unaccepted proposal.
    pub async fn reject_dispatch(&self, id: i64) -> Result<(), Error> {
        self.policy_write(PolicyMutation::Reject(id))
            .await
            .map(|_| ())
    }
    /// Retain fenced placement ownership but discard unaccepted accounting.
    pub async fn abandon_dispatch(&self, id: i64) -> Result<(), Error> {
        self.policy_write(PolicyMutation::Abandon(id))
            .await
            .map(|_| ())
    }
    /// Claim ephemeral completion metrics once across hook replay and restarts.
    pub async fn count_observation(&self, id: i64) -> Result<bool, Error> {
        Ok(self.policy_write(PolicyMutation::Count(id)).await? == 1)
    }
    /// Admission intents whose reply/accounting may have been interrupted.
    pub async fn pending_dispatches(&self) -> Result<Vec<i64>, Error> {
        Ok(sqlx::query_scalar(
            "SELECT job_id FROM dispatch_proposals WHERE accepted=0 ORDER BY job_id",
        )
        .fetch_all(&self.pool)
        .await?)
    }
    /// Idempotent durable hook handoff; no remote I/O delays admission or cleanup.
    pub async fn observe(
        &self,
        id: i64,
        reason: Option<String>,
        completed: bool,
        at: i64,
    ) -> Result<(), Error> {
        self.policy_write(PolicyMutation::Observe(id, reason, completed, at))
            .await
            .map(|_| ())
    }
    /// Pending work plus recent processed completions for rolling alerts.
    pub async fn observations(&self, since: i64) -> Result<Vec<Observation>, Error> {
        Ok(sqlx::query_as("SELECT * FROM controller_observations WHERE processed=0 OR observed_at>? ORDER BY job_id")
            .bind(since).fetch_all(&self.pool).await?)
    }
    /// Mark a fully reconciled observation after idempotent history/retry writes.
    pub async fn observation_result(
        &self,
        id: i64,
        infra: bool,
        processed: bool,
    ) -> Result<(), Error> {
        self.policy_write(PolicyMutation::Processed(id, infra, processed))
            .await
            .map(|_| ())
    }
    /// Remember release expiry derived from newer upstream release publication.
    pub async fn set_template_expiry(&self, version: String, at: i64) -> Result<(), Error> {
        self.policy_write(PolicyMutation::Expiry(version, at))
            .await
            .map(|_| ())
    }
    /// Enforced deadline for one runner version, when a newer release exists.
    pub async fn template_expiry(&self, version: &str) -> Result<Option<i64>, Error> {
        Ok(
            sqlx::query_scalar("SELECT expires_at FROM template_expiry WHERE version=?")
                .bind(version)
                .fetch_optional(&self.pool)
                .await?,
        )
    }
    async fn policy_write(&self, mutation: PolicyMutation) -> Result<i64, Error> {
        self.write(Mutation::Policy(mutation)).await
    }
}
