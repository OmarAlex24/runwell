use super::*;
use crate::availability::{Action, Change};

pub(super) fn timeline(
    trace: &PreparedTrace,
    runners: bool,
    allocation: Option<&[Vec<usize>]>,
    hosts: usize,
) -> Vec<Change> {
    let mut result = Vec::new();
    let mut periods = vec![trace.pool_limits.clone(); hosts];
    for change in &trace.availability {
        if let Action::Size {
            host,
            pool,
            runners,
        } = change.action
            && host < hosts
        {
            periods[host][pool] = periods[host][pool].max(runners);
        }
    }
    if runners && allocation.is_none() {
        result.extend(
            trace
                .runner_history
                .iter()
                .filter(|(_, h, _, _)| *h < hosts)
                .map(|&(time, host, pool, runners)| Change {
                    time,
                    action: Action::Size {
                        host,
                        pool,
                        runners,
                    },
                }),
        );
    }
    for c in &trace.availability {
        match c.action {
            Action::Size { host, .. } if host < hosts && runners && allocation.is_none() => {
                result.push(*c)
            }
            Action::Runner {
                host,
                pool,
                runner,
                offline,
            } if host < hosts && runners => {
                if let Some(counts) = allocation {
                    // Explicit counterfactual assumption: additional slots cycle
                    // the observed ordinal patterns on this host/pool. No inferred
                    // outages on hosts absent from the input. Installed-size history
                    // does not override the chosen static allocation.
                    let period = periods[host][pool].max(1);
                    for runner in (runner..counts[host][pool]).step_by(period) {
                        result.push(Change {
                            time: c.time,
                            action: Action::Runner {
                                host,
                                pool,
                                runner,
                                offline,
                            },
                        });
                    }
                } else {
                    result.push(*c);
                }
            }
            Action::Host { host, .. } if host < hosts => result.push(*c),
            _ => {}
        }
    }
    result
}
impl Engine<'_> {
    pub(super) fn refresh_pool(&mut self, host: usize, pool: usize) {
        self.nodes[host].free_runners[pool] = self.runner_slots[host][pool].free();
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
            match self.capacity_changes[event.job].action {
                Action::Size {
                    host,
                    pool,
                    runners,
                } => {
                    self.runner_slots[host][pool].set_limit(runners);
                    self.refresh_pool(host, pool);
                }
                Action::Runner {
                    host,
                    pool,
                    runner,
                    offline,
                } => {
                    if offline {
                        self.runner_slots[host][pool].offline(runner);
                    } else {
                        self.runner_slots[host][pool].online(runner);
                    }
                    self.refresh_pool(host, pool);
                }
                Action::Host { host, offline } => {
                    if offline {
                        self.host_offline[host] += 1;
                    } else {
                        self.host_offline[host] = self.host_offline[host].saturating_sub(1);
                    }
                }
            }
        }
    }
}
