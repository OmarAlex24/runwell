use runwell_admission::Resources;
use runwell_scheduler::*;

pub fn job(id: usize, repo: &str, pr: &str, snapshot: &mut ProductionSnapshot) -> PendingJob {
    snapshot.jobs.insert(
        id,
        JobIdentity {
            key: HistoryKey {
                repo: repo.into(),
                workflow_job: format!("ci/{id}"),
                class: "small".into(),
            },
            pull_request: pr.into(),
            run: pr.into(),
            criticality: Some(Criticality::default()),
        },
    );
    PendingJob {
        request_id: id,
        pool: 0,
        host_class: None,
        ready_at: 0.0,
        expected_seconds: 10.0,
        critical_path_seconds: 10.0,
        fair_service: 0.0,
        reservation: Resources {
            cpu_slots: 1,
            memory_bytes: 1,
        },
    }
}
pub fn node(id: usize, cpu: u32, memory: u64, snapshot: &mut ProductionSnapshot) -> NodeHeadroom {
    snapshot.nodes.insert(
        id,
        NodeStatus {
            admission_open: true,
            remaining_jobs: u32::MAX,
            classes: ["small".into()].into(),
            active_runs: Default::default(),
        },
    );
    NodeHeadroom {
        node_id: id,
        class: "any".into(),
        capacity: Resources {
            cpu_slots: cpu,
            memory_bytes: memory,
        },
        reserved: Resources::default(),
        free_runners: vec![100],
    }
}
