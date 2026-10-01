//! Preserve queue timestamps and workflow identity for controller scheduling adapters.
use crate::{Controller, Error};
use runwell_scaleset::Job;
use runwell_store::SchedulingContext;
impl Controller {
    pub(crate) async fn scheduling_metadata(
        &self,
        id: i64,
        event: &Job,
        replaced: bool,
    ) -> Result<(), Error> {
        let existing = if replaced {
            None
        } else {
            self.store.scheduling_context(id).await?
        };
        let mut context = existing.unwrap_or(SchedulingContext {
            ready_at_ms: self.store.queued_at(id).await?,
            ..Default::default()
        });
        if let Some(at) = event.queue_time {
            context.ready_at_ms = at.as_millisecond();
        }
        if context.workflow_job.is_empty() && !event.job_display_name.is_empty() {
            // The scale-set feed supplies a display name, not the YAML job key.
            // Namespace the fallback by workflow reference; DAG enrichers can
            // replace it with an exact key via Store::set_scheduling_context.
            context.workflow_job = format!(
                "display:{}:{}",
                event.job_workflow_ref, event.job_display_name
            );
        }
        self.store.set_scheduling_context(id, context).await?;
        Ok(())
    }
}
