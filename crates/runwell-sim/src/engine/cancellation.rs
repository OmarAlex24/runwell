use super::*;
impl Engine<'_> {
    pub(super) fn cancel_due(&mut self) {
        while self
            .cancel_agenda
            .peek()
            .is_some_and(|e| e.time <= self.now + EPS)
        {
            let Some(event) = self.cancel_agenda.pop() else {
                break;
            };
            let ids = self.trace.runs[event.job].jobs.clone();
            for i in ids {
                if self.timings[i].done {
                    continue;
                }
                self.cancelled_runs[event.job] = true;
                self.timings[i].cancelled = true;
                let active = self.running.contains(&i);
                if self.held.contains(&i) {
                    self.timings[i].semaphore_wait += self.now - self.timings[i].held_at;
                } else if !active {
                    self.timings[i].ready = self.now;
                    self.timings[i].start = self.now;
                }
                self.finish(i, active);
            }
        }
        self.ready.retain(|&i| !self.timings[i].done);
        self.held.retain(|&i| !self.timings[i].done);
        self.running.retain(|&i| !self.timings[i].done);
    }
}
