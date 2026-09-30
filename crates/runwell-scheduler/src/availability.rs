//! Pure persistent-runner slot accounting. Offline listeners drain active jobs.
use std::collections::{BTreeMap, BTreeSet};

/// Identity-aware slots: an offline busy runner must not also remove an idle slot.
#[derive(Debug, Clone, Default)]
pub struct RunnerSlots {
    limit: usize,
    occupied: BTreeSet<usize>,
    offline: BTreeMap<usize, usize>,
}
impl RunnerSlots {
    /// Initially online slots, numbered from zero.
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }
    /// Set installed capacity; jobs on removed slots drain before releasing them.
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
    }
    /// Begin an interval; nested or overlapping intervals count only once as loss.
    pub fn offline(&mut self, runner: usize) {
        *self.offline.entry(runner).or_default() += 1;
    }
    /// End an interval. An unmatched recovery is harmless.
    pub fn online(&mut self, runner: usize) {
        if let Some(count) = self.offline.get_mut(&runner) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.offline.remove(&runner);
            }
        }
    }
    fn available(&self, runner: usize) -> bool {
        !self.occupied.contains(&runner) && !self.offline.contains_key(&runner)
    }
    /// Free online slots within installed capacity.
    pub fn free(&self) -> usize {
        (0..self.limit)
            .filter(|&i| self.available(i))
            .count()
            .min(self.limit.saturating_sub(self.occupied.len()))
    }
    /// Reserve the lowest-numbered free online runner.
    pub fn acquire(&mut self) -> Option<usize> {
        if self.occupied.len() >= self.limit {
            return None;
        }
        let runner = (0..self.limit).find(|&i| self.available(i))?;
        self.occupied.insert(runner);
        Some(runner)
    }
    /// Release an active job, preserving any overlapping outage.
    pub fn release(&mut self, runner: usize) {
        self.occupied.remove(&runner);
    }
}

/// Placement sees only online hosts; existing reservations remain on paused hosts.
pub fn online_nodes(nodes: &[crate::NodeHeadroom], offline: &[usize]) -> Vec<crate::NodeHeadroom> {
    nodes
        .iter()
        .filter(|n| offline.get(n.node_id) == Some(&0))
        .cloned()
        .collect()
}

/// Opt-in sensitivity: resource admission plus legacy persistent-listener limits.
pub struct RunwellWithRunners(pub crate::Runwell);
impl crate::SchedulingPolicy for RunwellWithRunners {
    fn select(
        &self,
        jobs: &[crate::PendingJob],
        nodes: &[crate::NodeHeadroom],
        now: f64,
    ) -> Option<crate::Placement> {
        self.0.select_with_runners(jobs, nodes, now, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrinking_capacity_drains_even_when_removed_ordinals_remain_busy() {
        let mut slots = RunnerSlots::new(3);
        assert_eq!(slots.acquire(), Some(0));
        assert_eq!(slots.acquire(), Some(1));
        assert_eq!(slots.acquire(), Some(2));
        slots.release(0);
        slots.set_limit(1);
        assert_eq!(slots.free(), 0);
        assert_eq!(slots.acquire(), None);
        slots.release(1);
        assert_eq!(slots.acquire(), None);
        slots.release(2);
        assert_eq!(slots.free(), 1);
        assert_eq!(slots.acquire(), Some(0));
    }
}
