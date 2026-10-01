mod support;
use runwell_node::{NodeBackend, ProcessState};
use runwell_store::State;
use runwell_transport::{Key, Request, Rpc};
use std::sync::atomic::Ordering;
use support::Harness;

#[tokio::test]
async fn controller_killed_mid_job_re_adopts_without_second_jit() {
    let mut h = Harness::new().await;
    let a = h.queue(101).await;
    let b = h.queue(102).await;
    h.tick().await;
    assert_eq!(
        h.store
            .placements()
            .await
            .unwrap()
            .iter()
            .map(|p| &p.node_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
    h.restart_controller().await;
    h.tick().await;
    assert_eq!(h.api.creates.load(Ordering::SeqCst), 2);
    h.complete(a).await;
    h.complete(b).await;
    assert_eq!(h.store.job(a).await.unwrap().state, State::Completed);
    h.no_leaks().await;
}
#[tokio::test]
async fn node_daemon_killed_mid_job_preserves_units_and_reservations() {
    let mut h = Harness::new().await;
    let id = h.queue(101).await;
    h.tick().await;
    h.restart_node(0).await;
    h.tick().await;
    assert_eq!(h.hosts[0].state.lock().await.starts, 1);
    h.complete(id).await;
    h.no_leaks().await;
}
#[tokio::test]
async fn dockerd_restart_retains_teardown_then_cleans_all_resources() {
    let mut h = Harness::new().await;
    let id = h.queue(101).await;
    h.tick().await;
    h.hosts[0]
        .state
        .lock()
        .await
        .cleanup_failures
        .insert(id as u64);
    h.complete(id).await;
    assert!(!h.store.runner(id).await.unwrap().unwrap().cleaned);
    h.hosts[0].state.lock().await.cleanup_failures.clear();
    h.tick().await;
    assert_eq!(h.store.job(id).await.unwrap().state, State::Completed);
    h.no_leaks().await;
}
#[tokio::test]
async fn network_partition_reports_infra_once_and_reconnect_cleans_orphan() {
    let mut h = Harness::new().await;
    let id = h.queue(101).await;
    h.tick().await;
    h.links[0].cut.store(true, Ordering::SeqCst);
    h.clock.advance(100_000);
    h.tick().await;
    h.tick().await;
    assert_eq!(h.hook.events.lock().await.len(), 1);
    assert_eq!(h.hook.events.lock().await[0].2, "node_lost");
    assert_eq!(
        h.hosts[0].inspect(id as u64).await.unwrap(),
        ProcessState::Running
    );
    h.links[0].cut.store(false, Ordering::SeqCst);
    h.tick().await;
    h.no_leaks().await;
}
#[tokio::test]
async fn host_lost_mid_job_reports_infra_once_then_reconciles_disk_and_docker() {
    let mut h = Harness::new().await;
    let id = h.queue(101).await;
    h.tick().await;
    h.links[0].cut.store(true, Ordering::SeqCst);
    h.hosts[0].state.lock().await.processes.clear();
    h.clock.advance(100_000);
    h.tick().await;
    h.restart_node(0).await;
    h.links[0].cut.store(false, Ordering::SeqCst);
    h.tick().await;
    assert_eq!(h.store.job(id).await.unwrap().state, State::Orphaned);
    assert_eq!(h.hook.events.lock().await.len(), 1);
    h.no_leaks().await;
}
#[tokio::test]
async fn oom_uses_measured_infra_evidence_once_after_cleanup() {
    let mut h = Harness::new().await;
    let id = h.queue(101).await;
    h.tick().await;
    {
        let mut s = h.hosts[0].state.lock().await;
        s.processes
            .insert(id as u64, ProcessState::Exited(Some(137)));
        s.measurements.insert(
            id as u64,
            runwell_store::JobMeasurement {
                job_id: id,
                oom_kills: 1,
                ..Default::default()
            },
        );
    }
    h.tick().await;
    h.tick().await;
    h.tick().await;
    assert_eq!(h.hook.events.lock().await.len(), 1);
    assert_eq!(h.hook.events.lock().await[0].2, "oom");
    h.no_leaks().await;
}
#[tokio::test]
async fn disk_full_before_prepare_never_acquires_or_creates_a_runner() {
    let mut h = Harness::new().await;
    let id = h.queue(101).await;
    h.hosts[0]
        .state
        .lock()
        .await
        .prepare_failures
        .insert(id as u64);
    h.tick().await;
    h.tick().await;
    assert_eq!(h.api.creates.load(Ordering::SeqCst), 0);
    assert!(!h.store.job(id).await.unwrap().acquired);
    assert_eq!(h.hook.events.lock().await.len(), 1);
    h.no_leaks().await;
}
#[tokio::test]
async fn duplicate_and_out_of_order_start_cannot_resurrect_cleaned_job() {
    let mut h = Harness::new().await;
    let id = h.queue(101).await;
    h.links[0].lose_start.store(true, Ordering::SeqCst);
    h.tick().await;
    h.tick().await;
    h.complete(id).await;
    let start = Request::Start {
        key: Key {
            job_id: id,
            attempt: 1,
        },
        agent_id: 1000 + id,
        jit: "redacted".into(),
    };
    assert!(h.links[0].call(start).await.is_ok());
    assert_eq!(h.hosts[0].state.lock().await.starts, 1);
    h.no_leaks().await;
}
#[tokio::test]
async fn ambiguous_jit_post_is_looked_up_never_recreated() {
    let mut h = Harness::new().await;
    h.queue(101).await;
    h.api.lose_create.store(true, Ordering::SeqCst);
    h.tick().await;
    h.restart_controller().await;
    h.tick().await;
    assert_eq!(h.api.creates.load(Ordering::SeqCst), 1);
    h.no_leaks().await;
}
#[tokio::test]
async fn draining_node_finishes_jobs_and_rejects_new_work_across_restart() {
    let mut h = Harness::new().await;
    let id = h.queue(101).await;
    h.tick().await;
    h.links[0].call(Request::Drain).await.unwrap();
    h.restart_node(0).await;
    let second = h.queue(102).await;
    h.tick().await;
    assert_eq!(
        h.store
            .placements()
            .await
            .unwrap()
            .iter()
            .find(|p| p.job_id == second)
            .unwrap()
            .node_id,
        "node-2"
    );
    h.complete(id).await;
    h.complete(second).await;
    h.no_leaks().await;
}
#[tokio::test]
async fn watchdog_defers_destructive_cleanup_while_github_reports_busy() {
    let mut h = Harness::new().await;
    let id = h.queue(101).await;
    h.tick().await;
    h.api.busy.store(true, Ordering::SeqCst);
    h.clock.advance(7_300_000);
    h.tick().await;
    assert_eq!(h.hook.events.lock().await.len(), 1);
    assert_eq!(h.hook.events.lock().await[0].2, "duration_watchdog");
    assert_eq!(
        h.hosts[0].inspect(id as u64).await.unwrap(),
        ProcessState::Running
    );
    h.api.busy.store(false, Ordering::SeqCst);
    h.tick().await;
    h.no_leaks().await;
}

#[tokio::test]
async fn stale_reports_do_not_refresh_liveness_or_revive_fenced_attempts() {
    use runwell_transport::{Handler, Identity, Response};
    let mut h = Harness::new().await;
    let id = h.queue(101).await;
    h.tick().await;
    let Response::Report(report) = h.links[0].call(Request::Report).await.unwrap() else {
        panic!()
    };
    h.fleet
        .handle(Identity::node("node-1"), Request::Register(report.clone()))
        .await
        .unwrap();
    h.links[0].cut.store(true, Ordering::SeqCst);
    h.clock.advance(100_000);
    h.fleet
        .handle(Identity::node("node-1"), Request::Register(report.clone()))
        .await
        .unwrap();
    h.tick().await;
    assert_eq!(h.store.job(id).await.unwrap().state, State::Orphaned);
    assert!(
        h.fleet
            .handle(Identity::node("node-2"), Request::Register(report))
            .await
            .is_err()
    );
    h.links[0].cut.store(false, Ordering::SeqCst);
    h.tick().await;
    h.no_leaks().await;
}

#[tokio::test]
async fn healthy_node_reports_do_not_replace_missing_runner_heartbeat() {
    let mut h = Harness::new().await;
    h.queue(101).await;
    h.tick().await;
    h.withhold_heartbeat = true;
    h.clock.advance(185_000);
    h.tick().await;
    h.tick().await;
    assert_eq!(h.hook.events.lock().await.len(), 1);
    assert_eq!(h.hook.events.lock().await[0].2, "heartbeat_missing");
    assert!(
        h.store
            .node_health()
            .await
            .unwrap()
            .iter()
            .all(|(_, lost)| !lost)
    );
    h.tick().await;
    h.no_leaks().await;
}
