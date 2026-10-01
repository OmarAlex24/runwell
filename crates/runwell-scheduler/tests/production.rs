mod support;
use runwell_scheduler::*;
use support::*;
#[test]
fn critical_graph_then_short_history_then_p90() {
    let mut snapshot = ProductionSnapshot::default();
    let jobs = vec![
        job(0, "repo", "1", &mut snapshot),
        job(1, "repo", "1", &mut snapshot),
    ];
    let nodes = vec![node(0, 2, 2, &mut snapshot)];
    let config = ProductionConfig::default();
    let state = FairState::default();
    let shapes = graph_criticality(&[vec![], vec![0], vec![0], vec![1, 2]]).unwrap();
    assert_eq!(
        shapes[0],
        Criticality {
            depth: 2,
            fan_out: 3
        }
    );
    snapshot.jobs.get_mut(&1).unwrap().criticality = Some(shapes[0]);
    assert_eq!(
        Production::new(&config, &snapshot, &state)
            .unwrap()
            .select(&jobs, &nodes, 0.0)
            .unwrap()
            .request_id,
        1
    );
    snapshot.jobs.get_mut(&1).unwrap().criticality = None;
    let key = snapshot.jobs[&1].key.clone();
    snapshot.history.jobs.insert(
        key,
        DurationEstimate {
            p50_seconds: 1.0,
            p90_seconds: 5.0,
            samples: 2,
            criticality: Criticality::default(),
        },
    );
    assert_eq!(
        Production::new(&config, &snapshot, &state)
            .unwrap()
            .select(&jobs, &nodes, 0.0)
            .unwrap()
            .request_id,
        1
    );
}
#[test]
fn headroom_and_tight_run_anti_affinity_use_admission_snapshot() {
    let mut snapshot = ProductionSnapshot::default();
    let jobs = vec![job(0, "a", "run1", &mut snapshot)];
    let nodes = vec![node(0, 4, 4, &mut snapshot), node(1, 2, 2, &mut snapshot)];
    let config = ProductionConfig {
        tight_headroom: 0.8,
        ..Default::default()
    };
    let state = FairState::default();
    assert_eq!(
        Production::new(&config, &snapshot, &state)
            .unwrap()
            .select(&jobs, &nodes, 0.0)
            .unwrap()
            .node_id,
        0
    );
    snapshot
        .nodes
        .get_mut(&0)
        .unwrap()
        .active_runs
        .insert(("a".into(), "run1".into()));
    assert_eq!(
        Production::new(&config, &snapshot, &state)
            .unwrap()
            .select(&jobs, &nodes, 0.0)
            .unwrap()
            .node_id,
        1
    );
    snapshot.nodes.get_mut(&1).unwrap().admission_open = false;
    assert_eq!(
        Production::new(&config, &snapshot, &state)
            .unwrap()
            .select(&jobs, &nodes, 0.0)
            .unwrap()
            .node_id,
        0
    );
    snapshot.nodes.get_mut(&0).unwrap().classes.clear();
    assert!(
        Production::new(&config, &snapshot, &state)
            .unwrap()
            .select(&jobs, &nodes, 0.0)
            .is_none()
    );
}
#[test]
fn unknown_graph_uses_workflow_history_and_invalid_graph_is_rejected() {
    let mut snapshot = ProductionSnapshot::default();
    let jobs = vec![
        job(0, "a", "1", &mut snapshot),
        job(1, "a", "1", &mut snapshot),
    ];
    let nodes = vec![node(0, 1, 1, &mut snapshot)];
    snapshot.jobs.get_mut(&1).unwrap().criticality = None;
    let key = snapshot.jobs[&1].key.clone();
    snapshot.history.jobs.insert(
        key,
        DurationEstimate {
            p50_seconds: 100.0,
            p90_seconds: 200.0,
            samples: 1,
            criticality: Criticality {
                depth: 2,
                fan_out: 3,
            },
        },
    );
    assert_eq!(
        Production::new(
            &ProductionConfig::default(),
            &snapshot,
            &FairState::default()
        )
        .unwrap()
        .select(&jobs, &nodes, 0.0)
        .unwrap()
        .request_id,
        1
    );
    assert!(graph_criticality(&[vec![1], vec![0]]).is_err());
    assert!(graph_criticality(&[vec![2]]).is_err());
}
#[test]
fn impossible_oldest_job_does_not_hide_a_feasible_aged_job() {
    let mut snapshot = ProductionSnapshot::default();
    let mut huge = job(0, "a", "1", &mut snapshot);
    huge.reservation.cpu_slots = 100;
    let mut large = job(1, "a", "1", &mut snapshot);
    large.reservation.cpu_slots = 2;
    let mut tiny = job(2, "a", "1", &mut snapshot);
    tiny.ready_at = 300.0;
    let mut nodes = vec![node(0, 2, 10, &mut snapshot)];
    nodes[0].reserved.cpu_slots = 1;
    assert!(
        Production::new(
            &ProductionConfig::default(),
            &snapshot,
            &FairState::default()
        )
        .unwrap()
        .select(&[huge, large, tiny], &nodes, 300.0)
        .is_none()
    );
}
