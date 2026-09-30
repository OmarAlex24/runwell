use super::*;
impl Engine<'_> {
    pub(super) fn refresh_pool(&mut self, host: usize, pool: usize) {
        self.nodes[host].free_runners[pool] = runwell_scheduler::runner_headroom(
            self.runner_limits[host][pool],
            self.runner_occupied[host][pool],
        );
    }
    pub(super) fn capacity_due(&mut self) {
        while self
            .capacity_agenda
            .peek()
            .is_some_and(|e| e.time <= self.now + EPS)
        {
            let Some(event) = self.capacity_agenda.pop() else {
                break;
            };
            let (_, host, pool, limit) = self.trace.runner_history[event.job];
            self.runner_limits[host][pool] = limit;
            self.refresh_pool(host, pool);
        }
    }
}
