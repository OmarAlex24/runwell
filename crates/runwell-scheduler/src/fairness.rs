use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Caller-owned smooth weighted round-robin deficits, measured in admissions.
/// Persist the returned state only after a reservation is accepted. Inactive
/// queues accrue no credit. Aging overrides are charged to the same accounts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FairState {
    /// Repository deficits.
    pub repositories: BTreeMap<String, i128>,
    /// PR deficits, separately within each repository.
    pub pull_requests: BTreeMap<String, BTreeMap<String, i128>>,
}

pub(crate) fn accrue(
    state: &mut BTreeMap<String, i128>,
    weights: &BTreeMap<String, u32>,
) -> Option<String> {
    state.retain(|key, _| weights.contains_key(key));
    for (key, &weight) in weights {
        let score = state.entry(key.clone()).or_default();
        *score = score.saturating_add(i128::from(weight));
    }
    state
        .iter()
        .max_by(|(ka, a), (kb, b)| a.cmp(b).then(kb.cmp(ka)))
        .map(|(key, _)| key.clone())
}

pub(crate) fn charge(
    state: &mut BTreeMap<String, i128>,
    weights: &BTreeMap<String, u32>,
    selected: &str,
) {
    let total: i128 = weights.values().map(|&v| i128::from(v)).sum();
    if let Some(score) = state.get_mut(selected) {
        *score = score.saturating_sub(total);
    }
}
