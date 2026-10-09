use super::*;

impl Engine<'_> {
    pub(super) fn candidate(&self, i: usize) -> PendingJob {
        let j = &self.trace.jobs[i];
        // The event loop consumes arrivals through now + EPS. Normalize only
        // that tolerance here so a consumed wakeup stays eligible for a strict
        // production clock, while the recorded readiness remains unchanged.
        let ready = self.timings[i].ready;
        let ready_at = if ready <= self.now + EPS {
            ready.min(self.now)
        } else {
            ready
        };
        PendingJob {
            request_id: i,
            pool: j.pool,
            host_class: j.demand.host_class.clone(),
            ready_at,
            expected_seconds: j.work,
            critical_path_seconds: j.path,
            fair_service: self.service[j.repo],
            reservation: j.demand.resources(),
        }
    }
    pub(super) fn validate_placement(&self) -> Result<(), Error> {
        for (i, _) in self
            .trace
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.local && j.work > 0.0)
        {
            if self
                .choose(&[self.candidate(i)], &self.nodes, 0.0)?
                .is_none()
            {
                return Err(Error::Invalid(format!(
                    "job index {i} cannot fit an empty eligible host"
                )));
            }
        }
        Ok(())
    }
    pub(super) fn place(&mut self) -> Result<(), Error> {
        loop {
            let candidates: Vec<_> = self
                .ready
                .iter()
                .filter(|&&i| {
                    let j = &self.trace.jobs[i];
                    runwell_scheduler::parallel_slot(
                        j.max_parallel,
                        j.matrix_group.map_or(0, |g| self.matrix_active[g]),
                    )
                })
                .map(|&i| self.candidate(i))
                .collect();
            let online = runwell_scheduler::online_nodes(&self.nodes, &self.host_offline);
            let Some((p, fairness)) = self.choose(&candidates, &online, self.now)? else {
                break;
            };
            if let Some(fairness) = fairness {
                self.fair_state = fairness;
            }
            let (i, h) = (p.request_id, p.node_id);
            self.ready.retain(|&j| j != i);
            if let Some(group) = self.trace.jobs[i].matrix_group {
                self.matrix_active[group] += 1;
            }
            self.timings[i].host = Some(h);
            self.timings[i].start = self.now;
            if self.use_runners {
                let pool = self.trace.jobs[i].pool;
                self.timings[i].runner = Some(
                    self.runner_slots[h][pool]
                        .acquire()
                        .ok_or_else(|| Error::Invalid("selected runner is unavailable".into()))?,
                );
                self.refresh_pool(h, pool);
            }
            if self.policy == Policy::Baseline
                && let Some(gate) = self.trace.jobs[i].gate
                && runwell_scheduler::requires_heavy_slot(
                    true,
                    self.trace.jobs[i].observed_semaphore,
                    self.config.semaphore_history,
                )
                && self.gate_limit(gate, h).is_some()
            {
                self.timings[i].phase = Phase::Before;
                self.timings[i].remaining = self.trace.jobs[i].semaphore_work[0];
            } else {
                self.timings[i].remaining = self.trace.jobs[i].work;
            }
            self.start(i)?;
        }
        Ok(())
    }
    pub(super) fn start_held(&mut self) -> Result<(), Error> {
        let held = std::mem::take(&mut self.held);
        for i in held {
            let (Some(h), Some(gate)) = (self.timings[i].host, self.trace.jobs[i].gate) else {
                continue;
            };
            let Some(limit) = self.gate_limit(gate, h) else {
                self.start(i)?;
                continue;
            };
            let (timeout, poll) = {
                let g = self.config.gate(gate);
                (g.timeout, g.poll)
            };
            if self.host_offline[h] > 0 || (poll > 0.0 && self.now + EPS < self.timings[i].poll_at)
            {
                self.held.push(i);
                continue;
            }
            match heavy_slot(
                self.slots[gate][h],
                limit,
                self.now - self.timings[i].held_at + EPS,
                timeout,
            ) {
                SemaphoreDecision::Wait => {
                    let deadline = self.timings[i].held_at + timeout;
                    self.timings[i].poll_at = if poll > 0.0 {
                        (self.now + poll).min(deadline)
                    } else {
                        deadline
                    };
                    self.held.push(i);
                }
                decision => {
                    if decision == SemaphoreDecision::Acquire {
                        self.slots[gate][h] += 1;
                        self.timings[i].slot = true;
                    } else {
                        self.fail_opens += 1;
                    }
                    self.timings[i].semaphore_wait += self.now - self.timings[i].held_at;
                    self.timings[i].phase = Phase::Protected;
                    self.timings[i].remaining = self.trace.jobs[i].semaphore_work[1];
                    self.start(i)?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn start(&mut self, i: usize) -> Result<(), Error> {
        let t = &mut self.timings[i];
        if let Some(h) = t.host {
            let r = self.trace.jobs[i].demand.resources();
            self.nodes[h].reserved.cpu_slots = self.nodes[h]
                .reserved
                .cpu_slots
                .checked_add(r.cpu_slots)
                .ok_or_else(|| Error::Invalid("aggregate CPU demand overflow".into()))?;
            self.nodes[h].reserved.memory_bytes = self.nodes[h]
                .reserved
                .memory_bytes
                .checked_add(r.memory_bytes)
                .ok_or_else(|| Error::Invalid("aggregate memory demand overflow".into()))?;
        }
        self.running.push(i);
        Ok(())
    }
}
