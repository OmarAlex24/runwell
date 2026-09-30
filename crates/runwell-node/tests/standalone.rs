mod support;
use runwell_node::{NodeBackend, ProcessState};
use runwell_store::State;
use serde_json::json;
use support::*;
use wiremock::{Mock, ResponseTemplate, matchers::*};

#[tokio::test]
async fn admit_run_complete_duplicate_is_noop_and_ack_is_last() {
    let mut h = Harness::new(1).await;
    h.normal_registration().await;
    h.deliver(1, vec![available()]).await;
    h.deliver(2, vec![available()]).await;
    assert_eq!(count(&h.server, "POST", ACQUIRE).await, 1);
    assert_eq!(count(&h.server, "POST", JIT).await, 1);
    assert_eq!(h.backend.state.lock().await.starts, 1);
    let requests = h.server.received_requests().await.unwrap();
    let jit = requests.iter().position(|r| r.url.path() == JIT).unwrap();
    let ack = requests
        .iter()
        .position(|r| r.url.path() == "/queue/1")
        .unwrap();
    assert!(jit < ack);
    h.backend
        .state
        .lock()
        .await
        .processes
        .insert(1, ProcessState::Exited(Some(0)));
    h.deliver(3, vec![complete()]).await;
    assert_eq!(h.store.job(1).await.unwrap().state, State::Completed);
    assert!(h.store.runner(1).await.unwrap().unwrap().cleaned);
    assert_eq!(
        count(&h.server, "DELETE", &format!("{AGENTS}/1234")).await,
        1
    );
    let state = h.backend.state.lock().await;
    let sample = state
        .operations
        .iter()
        .position(|op| op == "measure:1")
        .unwrap();
    let cleanup = state
        .operations
        .iter()
        .position(|op| op == "cleanup:1")
        .unwrap();
    assert!(sample < cleanup);
    assert!(h.store.measurement(1).await.unwrap().is_some());
    drop(state);
    h.gateway.close().await.unwrap();
}
#[tokio::test]
async fn no_admission_leaves_github_queue_without_acquisition_or_runner() {
    let mut h = Harness::new(1).await;
    h.backend.state.lock().await.pressure.memory_full = 1.0;
    h.deliver(1, vec![available()]).await;
    assert_eq!(count(&h.server, "POST", ACQUIRE).await, 0);
    assert_eq!(count(&h.server, "POST", JIT).await, 0);
    assert!(h.store.runners().await.unwrap().is_empty());
    assert_eq!(h.store.job(1).await.unwrap().state, State::Queued);
    h.normal_registration().await;
    h.backend.state.lock().await.pressure.memory_full = 0.0;
    h.controller.tick(100_000).await.unwrap();
    h.controller.tick(131_000).await.unwrap();
    assert_eq!(count(&h.server, "POST", ACQUIRE).await, 1);
    h.gateway.close().await.unwrap();
}
#[tokio::test]
async fn restart_mid_job_readopts_without_creating_or_stopping_runner() {
    let mut h = Harness::new(1).await;
    h.normal_registration().await;
    h.deliver(1, vec![available()]).await;
    h.restart().await;
    h.controller.tick(40_000).await.unwrap();
    assert_eq!(h.backend.inspect(1).await.unwrap(), ProcessState::Running);
    assert_eq!(count(&h.server, "POST", JIT).await, 1);
    let mut second = available();
    second["runnerRequestId"] = json!(102);
    h.deliver(2, vec![second]).await;
    assert_eq!(count(&h.server, "POST", ACQUIRE).await, 1);
    assert!(
        h.backend
            .state
            .lock()
            .await
            .operations
            .iter()
            .all(|op| !op.starts_with("cleanup"))
    );
    h.gateway.close().await.unwrap();
}
#[tokio::test]
async fn busy_delete_keeps_slice_directory_reservation_and_retries() {
    let mut h = Harness::new(1).await;
    h.normal_registration().await;
    Mock::given(method("DELETE"))
        .and(path(format!("{AGENTS}/1234")))
        .respond_with(protocol::Sequence::new(vec![
            ResponseTemplate::new(409).set_body_json(fixture("busy")),
            ResponseTemplate::new(204),
        ]))
        .with_priority(1)
        .mount(&h.server)
        .await;
    h.deliver(1, vec![available()]).await;
    h.backend
        .state
        .lock()
        .await
        .processes
        .insert(1, ProcessState::Exited(Some(0)));
    h.deliver(2, vec![complete()]).await;
    assert!(!h.store.runner(1).await.unwrap().unwrap().cleaned);
    assert!(h.backend.state.lock().await.plans.contains_key(&1));
    assert!(h.store.measurement(1).await.unwrap().is_none());
    h.restart().await;
    assert!(h.store.measurement(1).await.unwrap().is_some());
    assert!(h.store.runner(1).await.unwrap().unwrap().cleaned);
    assert_eq!(
        count(&h.server, "DELETE", &format!("{AGENTS}/1234")).await,
        2
    );
    h.gateway.close().await.unwrap();
}
#[tokio::test]
async fn jit_agent_exists_collision_is_recovered_without_duplicate_local_start() {
    let mut h = Harness::new(1).await;
    Mock::given(method("POST"))
        .and(path(ACQUIRE))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"count": 1, "value": [101]})))
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path(JIT))
        .respond_with(protocol::Sequence::new(vec![
            ResponseTemplate::new(409).set_body_json(fixture("exists")),
            ResponseTemplate::new(200).set_body_json(jit()),
        ]))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(AGENTS))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"count":1,"value":[{"id":1234,"name":"rw-node-1-j1","runnerScaleSetId":42}]}),
        ))
        .with_priority(1)
        .mount(&h.server)
        .await;
    h.deliver(1, vec![available()]).await;
    assert_eq!(count(&h.server, "POST", JIT).await, 2);
    assert_eq!(
        count(&h.server, "DELETE", &format!("{AGENTS}/1234")).await,
        1
    );
    assert_eq!(h.backend.state.lock().await.starts, 1);
    h.gateway.close().await.unwrap();
}
#[tokio::test]
async fn oom_is_recorded_as_infrastructure_and_never_as_workflow_success() {
    let mut h = Harness::new(1).await;
    h.normal_registration().await;
    h.deliver(1, vec![available()]).await;
    {
        let mut state = h.backend.state.lock().await;
        state.processes.insert(1, ProcessState::Exited(Some(137)));
        state.measurements.insert(
            1,
            runwell_store::JobMeasurement {
                job_id: 1,
                oom_kills: 1,
                memory_peak: 9000,
                ..Default::default()
            },
        );
    }
    h.controller.tick(40_000).await.unwrap();
    let sample = h.store.measurement(1).await.unwrap().unwrap();
    assert!(sample.infra_signal);
    assert_eq!(sample.oom_kills, 1);
    assert_eq!(h.store.job(1).await.unwrap().state, State::Failed);
    h.gateway.close().await.unwrap();
}
