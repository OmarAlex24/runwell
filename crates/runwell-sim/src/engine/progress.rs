use super::*;

impl Engine<'_> {
    pub(super) fn complete(&mut self) {
        let running = std::mem::take(&mut self.running);
        for i in running {
            if self.timings[i].remaining > EPS {
                self.running.push(i);
                continue;
            }
            match self.timings[i].phase {
                Phase::Before => {
                    if let Some(h) = self.timings[i].host {
                        let r = self.trace.jobs[i].demand.resources();
                        self.nodes[h].reserved.cpu_slots -= r.cpu_slots;
                        self.nodes[h].reserved.memory_bytes -= r.memory_bytes;
                    }
                    self.timings[i].held_at = self.now;
                    self.timings[i].poll_at = self.now;
                    self.held.push(i);
                }
                Phase::Protected => {
                    let t = &mut self.timings[i];
                    if t.slot {
                        if let (Some(h), Some(gate)) = (t.host, self.trace.jobs[i].gate) {
                            self.slots[gate][h] -= 1;
                        }
                        t.slot = false;
                    }
                    t.phase = Phase::Whole;
                    t.remaining = self.trace.jobs[i].semaphore_work[2];
                    self.running.push(i);
                }
                Phase::Whole => self.finish(i, true),
            }
        }
    }
    pub(super) fn finish(&mut self, i: usize, was_running: bool) {
        let t = &mut self.timings[i];
        if t.done {
            return;
        }
        t.done = true;
        t.end = self.now;
        let p = t.failure_exposure / t.elapsed.max(EPS);
        t.failure = !t.cancelled && uniform(self.config.seed, i as u64) < p;
        if let Some(h) = t.host {
            let j = &self.trace.jobs[i];
            if let Some(group) = j.matrix_group {
                self.matrix_active[group] -= 1;
            }
            if was_running {
                let r = j.demand.resources();
                self.nodes[h].reserved.cpu_slots -= r.cpu_slots;
                self.nodes[h].reserved.memory_bytes -= r.memory_bytes;
            }
            if let Some(runner) = t.runner {
                self.runner_slots[h][j.pool].release(runner);
                self.nodes[h].free_runners[j.pool] = self.runner_slots[h][j.pool].free();
            }
            if t.slot
                && let Some(gate) = j.gate
            {
                self.slots[gate][h] -= 1;
            }
        }
        self.learn_completion(i);
        self.finished += 1;
        for &c in &self.children[i] {
            if self.timings[c].done {
                continue;
            }
            self.pending[c] -= 1;
            self.parent_end[c] = self.parent_end[c].max(self.now);
            if self.pending[c] == 0 {
                self.agenda.push(Event {
                    time: self.parent_end[c] + self.trace.jobs[c].delay,
                    job: c,
                });
            }
        }
    }
    pub(super) fn speeds(&self) -> Vec<f64> {
        self.running
            .iter()
            .map(|&i| {
                let Some(h) = self.timings[i].host else {
                    return 1.0;
                };
                if self.host_offline[h] > 0 {
                    return 0.0;
                }
                if self.config.observed_work {
                    return 1.0;
                }
                let n = &self.nodes[h];
                let fit = if self.config.fit_by_class {
                    &self.trace.classes[self.trace.jobs[i].class].fit
                } else {
                    &self.trace.fit
                };
                fit.speed(
                    f64::from(n.reserved.cpu_slots),
                    f64::from(n.capacity.cpu_slots),
                    n.reserved.memory_bytes as f64 / n.capacity.memory_bytes as f64,
                    self.config.memory_threshold,
                    self.config.memory_penalty,
                )
            })
            .collect()
    }
    pub(super) fn advance(&mut self, dt: f64, speeds: &[f64]) {
        for n in self
            .nodes
            .iter()
            .filter(|n| self.host_offline[n.node_id] == 0)
        {
            self.cpu_area += f64::from(n.reserved.cpu_slots.min(n.capacity.cpu_slots)) * dt;
            self.memory_area += n.reserved.memory_bytes.min(n.capacity.memory_bytes) as f64 * dt;
        }
        for (&i, &speed) in self.running.iter().zip(speeds) {
            let t = &mut self.timings[i];
            let job = &self.trace.jobs[i];
            t.remaining -= dt * speed;
            if let Some(h) = t.host {
                if self.host_offline[h] > 0 {
                    continue;
                }
                self.service[job.repo] += dt * f64::from(job.demand.cores);
                if job.demand.heavy {
                    let n = &self.nodes[h];
                    let fit = if self.config.fit_by_class {
                        &self.trace.classes[job.class].fit
                    } else {
                        &self.trace.fit
                    };
                    let equivalent = f64::from(n.reserved.cpu_slots)
                        / f64::from(n.capacity.cpu_slots)
                        * fit.reference_cores
                        / fit.cores_per_job;
                    let memory = n.reserved.memory_bytes as f64 / n.capacity.memory_bytes as f64;
                    let stress = ((equivalent - self.config.low_concurrency)
                        / (self.config.high_concurrency - self.config.low_concurrency))
                        .max(memory / self.config.memory_threshold - 1.0)
                        .clamp(0.0, 1.0);
                    let probability = fit.low_failure_rate
                        + (fit.high_failure_rate - fit.low_failure_rate).max(0.0) * stress;
                    t.failure_exposure += dt * probability;
                    t.elapsed += dt;
                }
            }
        }
    }
}

// SplitMix64 gives each job a common random draw across all policy comparisons.
fn uniform(seed: u64, job: u64) -> f64 {
    let mut x = seed.wrapping_add(job.wrapping_mul(0x9e3779b97f4a7c15));
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    ((x ^ (x >> 31)) >> 11) as f64 / ((1_u64 << 53) as f64)
}
