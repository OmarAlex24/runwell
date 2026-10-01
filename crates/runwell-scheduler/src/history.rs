use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Stable workflow job identity, independent of display names and matrix values.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HistoryKey {
    /// Canonical owner/repository (lowercase).
    pub repo: String,
    /// Workflow path plus YAML job key; include a revision epoch if desired.
    pub workflow_job: String,
    /// Reservation class.
    pub class: String,
}

/// Windowed successful execution estimates. Queue time is never intrinsic work.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DurationEstimate {
    /// Median execution seconds.
    pub p50_seconds: f64,
    /// 90th percentile execution seconds.
    pub p90_seconds: f64,
    /// Successful samples in the window; zero denotes a class default.
    pub samples: u32,
    /// Last known downstream shape, used when a run graph is unavailable.
    pub criticality: crate::Criticality,
}

/// Cheap in-memory snapshot loaded once and refreshed after completions.
#[derive(Debug, Clone, Default)]
pub struct HistorySnapshot {
    /// Estimates by repository, workflow job and class.
    pub jobs: BTreeMap<HistoryKey, DurationEstimate>,
    /// Operator-provided cold-start estimates by class.
    pub class_defaults: BTreeMap<String, DurationEstimate>,
}
impl HistorySnapshot {
    /// Resolve learned history, then a class default. Missing defaults are explicit.
    pub fn estimate(&self, key: &HistoryKey) -> Option<DurationEstimate> {
        self.jobs
            .get(key)
            .filter(|e| e.samples > 0)
            .or_else(|| self.class_defaults.get(&key.class))
            .copied()
    }
}
