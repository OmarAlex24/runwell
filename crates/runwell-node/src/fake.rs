//! In-memory host implementation for macOS-compatible lifecycle tests.
use crate::*;
use runwell_admission::Pressure;
use runwell_runner::LaunchSpec;
use runwell_store::JobMeasurement;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

/// Inspectable host state; never stores JIT material.
#[derive(Default)]
pub struct FakeState {
    /// Planned slices/directories, keyed by durable identity.
    pub plans: BTreeMap<u64, JobPlan>,
    /// Process states.
    pub processes: BTreeMap<u64, ProcessState>,
    /// Final measurements to return.
    pub measurements: BTreeMap<u64, JobMeasurement>,
    /// Current host pressure.
    pub pressure: Pressure,
    /// Successful first starts.
    pub starts: usize,
    /// Optional promoted template version.
    pub template_version: Option<String>,
    /// Ordered host operations for safety assertions.
    pub operations: Vec<String>,
    /// Jobs whose simulated Docker cleanup fails until removed from this set.
    pub cleanup_failures: std::collections::BTreeSet<u64>,
    /// Inject a workspace reconciliation failure.
    pub workspace_reconcile_failure: bool,
    /// Last identities protected from workspace reconciliation.
    pub retained_workspaces: std::collections::HashSet<u64>,
}
/// Cloneable backend surviving controller reconstruction in restart tests.
#[derive(Clone, Default)]
pub struct FakeBackend {
    /// Shared simulated systemd/filesystem state.
    pub state: Arc<Mutex<FakeState>>,
}
impl NodeBackend for FakeBackend {
    fn initialize(&self) -> NodeFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn pressure(&self) -> NodeFuture<'_, Pressure> {
        Box::pin(async { Ok(self.state.lock().await.pressure) })
    }
    fn template_version(&self) -> NodeFuture<'_, String> {
        Box::pin(async {
            Ok(self
                .state
                .lock()
                .await
                .template_version
                .clone()
                .unwrap_or_else(|| "2.337.0".into()))
        })
    }
    fn prepare<'a>(&'a self, plan: &'a JobPlan) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            plan.slice.validate()?;
            let mut state = self.state.lock().await;
            if state
                .plans
                .values()
                .any(|p| p.directory == plan.directory && p.slice.job_id != plan.slice.job_id)
            {
                return Err(Error::Config);
            }
            state
                .operations
                .push(format!("prepare:{}", plan.slice.job_id));
            state.plans.insert(plan.slice.job_id, plan.clone());
            Ok(())
        })
    }
    fn harvest_workspace<'a>(&'a self, job: &'a runwell_store::Job) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            self.state
                .lock()
                .await
                .operations
                .push(format!("harvest:{}", job.id));
            Ok(())
        })
    }
    fn reconcile_workspaces<'a>(
        &'a self,
        retained: &'a std::collections::HashSet<u64>,
    ) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            state.operations.push("reconcile_workspaces".into());
            state.retained_workspaces.clone_from(retained);
            if state.workspace_reconcile_failure {
                Err(Error::Io)
            } else {
                Ok(())
            }
        })
    }
    fn start<'a>(&'a self, plan: &'a JobPlan, launch: &'a LaunchSpec) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            if !state.plans.contains_key(&plan.slice.job_id) || launch.install_dir != plan.directory
            {
                return Err(Error::Config);
            }
            if state.processes.contains_key(&plan.slice.job_id) {
                return Ok(());
            }
            state.starts += 1;
            state
                .operations
                .push(format!("start:{}", plan.slice.job_id));
            state
                .processes
                .insert(plan.slice.job_id, ProcessState::Running);
            Ok(())
        })
    }
    fn inspect(&self, id: u64) -> NodeFuture<'_, ProcessState> {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .await
                .processes
                .get(&id)
                .copied()
                .unwrap_or(ProcessState::Absent))
        })
    }
    fn measure(&self, id: u64) -> NodeFuture<'_, JobMeasurement> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            state.operations.push(format!("measure:{id}"));
            Ok(state
                .measurements
                .get(&id)
                .cloned()
                .unwrap_or(JobMeasurement {
                    job_id: id as i64,
                    ..Default::default()
                }))
        })
    }
    fn stop_runner(&self, id: u64) -> NodeFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            state.operations.push(format!("stop:{id}"));
            if state.cleanup_failures.contains(&id) {
                return Err(Error::Io);
            }
            state.processes.insert(id, ProcessState::Exited(None));
            Ok(())
        })
    }
    fn inventory(&self) -> NodeFuture<'_, Vec<LocalJob>> {
        Box::pin(async {
            Ok(self
                .state
                .lock()
                .await
                .plans
                .keys()
                .map(|id| LocalJob { id: *id })
                .collect())
        })
    }
    fn cleanup(&self, id: u64) -> NodeFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            state.operations.push(format!("cleanup:{id}"));
            if state.cleanup_failures.contains(&id) {
                return Err(Error::Io);
            }
            state.plans.remove(&id);
            state.processes.remove(&id);
            Ok(())
        })
    }
}
