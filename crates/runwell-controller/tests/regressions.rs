mod support;
use runwell_node::{NodeBackend, ProcessState};
use runwell_store::State;
use runwell_transport::{Clock, Key, Request, Response, Rpc};
use std::sync::atomic::Ordering;
use support::Harness;

#[tokio::test]
async fn lost_start_reply_then_process_exit_replays_success_across_node_restart() {
    let mut h = Harness::new().await;
    let id = h.queue(101).await;
    h.links[0].lose_start.store(true, Ordering::SeqCst);
    h.tick().await;
    assert_eq!(h.store.job(id).await.unwrap().state, State::RunnerCreated);
    h.hosts[0]
        .state
        .lock()
        .await
        .processes
        .insert(id as u64, ProcessState::Exited(Some(0)));
    h.restart_node(0).await;
    let plan = h.hosts[0].state.lock().await.plans[&(id as u64)].clone();
    h.fleet
        .start(
            &plan,
            &runwell_runner::LaunchSpec {
                agent_id: 1000 + id,
                install_dir: plan.directory.clone(),
                jit_config: secrecy::SecretString::from("lost-reply-retry"),
            },
        )
        .await
        .unwrap();
    assert_eq!(h.hosts[0].state.lock().await.starts, 1);
    assert!(h.store.failures().await.unwrap().is_empty());
    h.complete(id).await;
    assert!(h.hook.events.lock().await.is_empty());
    h.no_leaks().await;
}

#[tokio::test]
async fn ambiguous_start_without_process_evidence_never_relaunches() {
    let h = Harness::new().await;
    let id = h.queue(101).await;
    h.stores[0]
        .lease(runwell_store::NodeLease {
            job: h.store.job(id).await.unwrap(),
            attempt: 1,
            phase: runwell_transport::STARTING,
            plan: None,
            measurement: None,
            started_at_ms: Some(h.clock.now_ms()),
            heartbeat_at_ms: None,
        })
        .await
        .unwrap();
    assert!(matches!(
        h.links[0]
            .call(Request::Start {
                key: Key {
                    job_id: id,
                    attempt: 1
                },
                agent_id: 1000 + id,
                jit: "unused".into(),
            })
            .await,
        Err(runwell_transport::Error::Uncertain)
    ));
    assert_eq!(h.hosts[0].state.lock().await.starts, 0);
}

#[tokio::test]
async fn cancelled_lost_admission_reply_releases_bare_reservation_after_restarts() {
    let mut h = Harness::new().await;
    let id = h.queue(101).await;
    h.links[0].lose_admit.store(true, Ordering::SeqCst);
    h.tick().await;
    h.tick().await;
    assert_eq!(h.store.job(id).await.unwrap().state, State::Queued);
    assert!(h.store.runner(id).await.unwrap().is_none());
    assert_eq!(h.stores[0].active_leases().await.unwrap().len(), 1);
    h.store.transition(id, State::Orphaned).await.unwrap();
    h.restart_controller().await;
    h.restart_node(0).await;
    let operations = h.hosts[0].state.lock().await.operations.clone();
    h.tick().await;
    h.tick().await;
    assert!(h.stores[0].active_leases().await.unwrap().is_empty());
    assert_eq!(h.api.creates.load(Ordering::SeqCst), 0);
    assert!(
        h.hook.events.lock().await.is_empty(),
        "cancellation is not infrastructure failure"
    );
    assert_eq!(
        h.hosts[0].state.lock().await.operations,
        operations,
        "bare reservations have no cgroup to measure or stop"
    );
    h.restart_node(0).await;
    let Response::Report(report) = h.links[0].call(Request::Report).await.unwrap() else {
        panic!()
    };
    assert_eq!(
        (
            report.reserved_cpu,
            report.reserved_memory,
            report.free_slots
        ),
        (0, 0, 1)
    );
    let state = h.hosts[0].state.lock().await;
    assert!(
        state.plans.is_empty()
            && state.mounts.is_empty()
            && state.containers.is_empty()
            && state.processes.is_empty()
    );
    assert!(h.api.runners.lock().await.is_empty());
}

#[tokio::test]
async fn slow_preparation_does_not_consume_execution_watchdog_budget() {
    let mut h = Harness::new().await;
    let settings = h.config.network.as_mut().unwrap();
    settings.expected_seconds = 10;
    settings.watchdog_multiple = 2;
    settings.rpc_seconds = 30;
    settings.preparation_seconds = 60;
    h.restart_controller().await;
    let id = h.queue(101).await;
    let assigned = h.clock.now_ms();
    h.links[0].prepare_ms.store(25_000, Ordering::SeqCst);
    h.tick().await;
    let started = h
        .store
        .placement(id)
        .await
        .unwrap()
        .unwrap()
        .execution_started_at
        .unwrap();
    assert_eq!(started - assigned, 25_000);
    h.restart_controller().await;
    h.restart_node(0).await;
    h.tick().await;
    h.clock.advance(19_000);
    h.tick().await;
    assert_eq!(h.store.job(id).await.unwrap().state, State::Running);
    assert!(h.hook.events.lock().await.is_empty());
    h.clock.advance(2_000);
    h.tick().await;
    assert_eq!(h.hook.events.lock().await[0].2, "duration_watchdog");
    h.no_leaks().await;
}

#[tokio::test]
async fn preparation_has_its_own_durable_deadline() {
    let mut h = Harness::new().await;
    h.config.network.as_mut().unwrap().preparation_seconds = 30;
    h.restart_controller().await;
    h.queue(101).await;
    h.links[0].lose_admit.store(true, Ordering::SeqCst);
    h.tick().await;
    h.restart_controller().await;
    h.clock.advance(31_000);
    h.tick().await;
    h.tick().await;
    assert_eq!(h.hook.events.lock().await.len(), 1);
    assert_eq!(h.hook.events.lock().await[0].2, "preparation_timeout");
    h.no_leaks().await;
}
