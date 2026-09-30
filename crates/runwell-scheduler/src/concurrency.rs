//! Workflow matrix and cancel-in-progress rules shared with live scheduling.

/// Whether a workflow matrix still has dispatch capacity.
pub fn parallel_slot(limit: Option<usize>, active: usize) -> bool {
    limit.is_none_or(|max| active < max)
}

/// Identity and arrival of a run participating in workflow concurrency.
#[derive(Debug, Clone, Copy)]
pub struct RunArrival {
    /// Stable workflow run ID; rerun attempts do not supersede their own run.
    pub run_id: u64,
    /// Repository/workflow/PR-or-branch group index, absent if cancellation is off.
    pub group: Option<usize>,
    /// Arrival in seconds on the caller's clock.
    pub time: f64,
}

/// Deadline at the next distinct run in each cancellation group, input-order indexed.
pub fn cancel_deadlines(runs: &[RunArrival]) -> Vec<Option<f64>> {
    let mut order: Vec<_> = (0..runs.len()).collect();
    order.sort_by(|&a, &b| {
        runs[a]
            .time
            .total_cmp(&runs[b].time)
            .then(runs[a].run_id.cmp(&runs[b].run_id))
    });
    let mut next = std::collections::BTreeMap::new();
    let mut deadlines = vec![None; runs.len()];
    for i in order.into_iter().rev() {
        if let Some(group) = runs[i].group {
            if let Some(&(id, time, deadline)) = next.get(&group) {
                if id != runs[i].run_id {
                    deadlines[i] = Some(time);
                } else {
                    deadlines[i] = deadline;
                }
            }
            next.insert(group, (runs[i].run_id, runs[i].time, deadlines[i]));
        }
    }
    deadlines
}

/// Historical gate evidence applies during baseline reconstruction; absent
/// evidence uses the configured class, as a live daemon would.
pub fn requires_heavy_slot(heavy: bool, observed: Option<bool>, honor_history: bool) -> bool {
    heavy && (!honor_history || observed.unwrap_or(true))
}

/// Headroom remains zero while a reduced runner limit drains existing jobs.
pub fn runner_headroom(limit: usize, occupied: usize) -> usize {
    limit.saturating_sub(occupied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_inherits_next_distinct_run_deadline_without_crossing_groups() {
        let runs = [
            (1, Some(0), 0.),
            (1, Some(0), 2.),
            (3, Some(1), 3.),
            (2, Some(0), 4.),
            (4, None, 5.),
        ]
        .map(|(run_id, group, time)| RunArrival {
            run_id,
            group,
            time,
        });
        assert_eq!(
            cancel_deadlines(&runs),
            [Some(4.), Some(4.), None, None, None]
        );
        assert!(!requires_heavy_slot(true, Some(false), true));
        assert!(requires_heavy_slot(true, Some(false), false));
        assert_eq!(runner_headroom(2, 3), 0);
    }
}
