use crate::Fleet;
use runwell_admission::Resources;
use runwell_node::Error;
use runwell_scheduler::{NodeHeadroom, PendingJob};
use runwell_store::{Job, Placement};
use std::collections::BTreeMap;

impl Fleet {
    pub(crate) async fn available(
        &self,
    ) -> Result<Vec<(String, runwell_transport::Report)>, Error> {
        let now = self.clock.now_ms();
        let limit = self
            .config
            .network
            .as_ref()
            .ok_or(Error::Config)?
            .lost_seconds as i64
            * 1000;
        Ok(self
            .reports()
            .await?
            .into_iter()
            .filter(|(_, time, report)| {
                now.saturating_sub(*time) < limit && !report.draining && report.free_slots > 0
            })
            .map(|(id, _, report)| (id, report))
            .collect())
    }
    pub(crate) async fn select(&self, jobs: &[Job]) -> Result<Vec<i64>, Error> {
        if self.production.is_some() {
            return self.production_select(jobs).await;
        }
        let existing = self.store.placements().await?;
        let mut chosen: Vec<_> = jobs
            .iter()
            .filter(|j| existing.iter().any(|p| p.job_id == j.id && !p.lost))
            .map(|j| j.id)
            .collect();
        let reports = self.available().await?;
        let network = self.config.network.as_ref().ok_or(Error::Config)?;
        let now = self.clock.now_ms();
        let mut nodes: Vec<_> = reports
            .iter()
            .enumerate()
            .map(|(index, (id, r))| NodeHeadroom {
                node_id: index,
                class: network
                    .nodes
                    .iter()
                    .find(|n| &n.id == id)
                    .map_or(String::new(), |n| n.class.clone()),
                capacity: Resources {
                    cpu_slots: r.cpu_slots,
                    memory_bytes: r.memory_bytes,
                },
                reserved: Resources {
                    cpu_slots: r.reserved_cpu,
                    memory_bytes: r.reserved_memory,
                },
                free_runners: vec![r.free_slots as usize],
            })
            .collect();
        // Durable but not yet reported reservations still consume optimistic
        // headroom. Node admission remains authoritative under races.
        for placement in &existing {
            if let Some((index, (_, report))) = reports
                .iter()
                .enumerate()
                .find(|(_, (id, _))| id == &placement.node_id)
                && !report.jobs.iter().any(|s| s.key.job_id == placement.job_id)
                && let Some(job) = jobs.iter().find(|j| j.id == placement.job_id)
            {
                nodes[index].reserved.cpu_slots = nodes[index]
                    .reserved
                    .cpu_slots
                    .saturating_add(job.metadata.reserved_cpu);
                nodes[index].reserved.memory_bytes = nodes[index]
                    .reserved
                    .memory_bytes
                    .saturating_add(job.metadata.reserved_memory);
                nodes[index].free_runners[0] = nodes[index].free_runners[0].saturating_sub(1);
            }
        }
        let mut pending: Vec<_> = jobs
            .iter()
            .filter(|j| !existing.iter().any(|p| p.job_id == j.id))
            .map(|j| PendingJob {
                request_id: j.id as usize,
                pool: 0,
                host_class: None,
                ready_at: 0.0,
                expected_seconds: network.expected_seconds as f64,
                critical_path_seconds: network.expected_seconds as f64,
                fair_service: 0.0,
                reservation: Resources {
                    cpu_slots: j.metadata.reserved_cpu,
                    memory_bytes: j.metadata.reserved_memory,
                },
            })
            .collect();
        while let Some(placement) = self.policy.select(&pending, &nodes, now as f64 / 1000.0) {
            let job = pending
                .iter()
                .find(|j| j.request_id == placement.request_id)
                .ok_or(Error::Config)?;
            let node = nodes
                .iter_mut()
                .find(|n| n.node_id == placement.node_id)
                .ok_or(Error::Config)?;
            let id = reports.get(node.node_id).ok_or(Error::Config)?.0.clone();
            self.store
                .place(Placement {
                    job_id: job.request_id as i64,
                    attempt: 1,
                    node_id: id,
                    assigned_at: now,
                    lost: false,
                    execution_started_at: None,
                })
                .await?;
            chosen.push(job.request_id as i64);
            node.reserved.cpu_slots = node
                .reserved
                .cpu_slots
                .saturating_add(job.reservation.cpu_slots);
            node.reserved.memory_bytes = node
                .reserved
                .memory_bytes
                .saturating_add(job.reservation.memory_bytes);
            node.free_runners[0] = node.free_runners[0].saturating_sub(1);
            pending.retain(|j| j.request_id != placement.request_id);
            nodes.retain(|n| n.free_runners[0] > 0);
        }
        Ok(chosen)
    }
    pub(crate) async fn capacities(
        &self,
        classes: &BTreeMap<i64, runwell_config::JobClass>,
    ) -> Result<BTreeMap<i64, u32>, Error> {
        let reports = self.available().await?;
        let mut capacities = BTreeMap::new();
        for (set, class) in classes {
            let mut capacity = 0u32;
            for (_, report) in &reports {
                let cpu = report.cpu_slots.saturating_sub(report.reserved_cpu) / class.cpu_slots;
                let memory = (report.memory_bytes.saturating_sub(report.reserved_memory)
                    / class.memory_high_bytes)
                    .min(u64::from(u32::MAX)) as u32;
                capacity = capacity.saturating_add(cpu.min(memory).min(report.free_slots));
            }
            let jobs = self.store.jobs().await?;
            let active = self
                .store
                .runners()
                .await?
                .iter()
                .filter(|r| {
                    !r.cleaned
                        && jobs
                            .iter()
                            .any(|j| j.id == r.job_id && j.metadata.scale_set_id == *set)
                })
                .count() as u32;
            capacities.insert(*set, capacity.saturating_add(active));
        }
        Ok(capacities)
    }
}
