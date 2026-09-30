mod support;
use runwell_node::{Drain, ProcessState, run_loop};
use runwell_store::State;
use serde_json::json;
use std::time::Duration;
use support::*;
use wiremock::{Mock, ResponseTemplate, matchers::*};

#[tokio::test]
async fn second_sigterm_closes_sessions_without_killing_running_job() {
    let mut h = Harness::new(1).await;
    h.normal_registration().await;
    h.deliver(1, vec![available()]).await;
    Mock::given(method("GET"))
        .and(path("/queue"))
        .respond_with(ResponseTemplate::new(202).set_delay(Duration::from_secs(30)))
        .mount(&h.server)
        .await;
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    tx.send(Drain::Graceful).await.unwrap();
    tx.send(Drain::Graceful).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        run_loop(&mut h.controller, h.gateway.clone(), rx),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(h.controller.is_draining());
    assert_eq!(
        h.backend.state.lock().await.processes[&1],
        ProcessState::Running
    );
    assert_eq!(count(&h.server, "DELETE", SESSION).await, 1);
    assert_eq!(
        count(&h.server, "DELETE", &format!("{AGENTS}/1234")).await,
        0
    );
}
#[tokio::test]
async fn graceful_drain_finishes_running_job_and_rejects_new_demand() {
    let mut h = Harness::new(1).await;
    h.normal_registration().await;
    h.deliver(1, vec![available()]).await;
    h.controller.drain();
    let mut second = available();
    second["runnerRequestId"] = json!(102);
    h.backend
        .state
        .lock()
        .await
        .processes
        .insert(1, ProcessState::Exited(Some(0)));
    h.deliver(2, vec![second, complete()]).await;
    assert!(h.controller.is_idle().await.unwrap());
    assert_eq!(h.store.job(2).await.unwrap().state, State::Queued);
    assert_eq!(count(&h.server, "POST", ACQUIRE).await, 1);
    h.gateway.close().await.unwrap();
}
#[tokio::test]
async fn crash_after_jit_before_id_persistence_deletes_by_durable_name() {
    let mut h = Harness::new(1).await;
    Mock::given(method("POST"))
        .and(path(ACQUIRE))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"count": 1, "value": [101]})))
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path(JIT))
        .respond_with(ResponseTemplate::new(500).set_body_string("synthetic-jit-secret"))
        .mount(&h.server)
        .await;
    let message = h.message(1, vec![available()]).await;
    let error = h.controller.handle(42, &message, 40_000).await.unwrap_err();
    assert!(!format!("{error} {error:?}").contains("synthetic-jit-secret"));
    assert_eq!(count(&h.server, "DELETE", "/queue/1").await, 0);
    Mock::given(method("GET"))
        .and(path(AGENTS))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"count":1,"value":[{"id":1234,"name":"rw-node-1-j1","runnerScaleSetId":42}]}),
        ))
        .with_priority(1)
        .mount(&h.server)
        .await;
    h.restart().await;
    assert_eq!(
        count(&h.server, "DELETE", &format!("{AGENTS}/1234")).await,
        1
    );
    assert!(h.store.runner(1).await.unwrap().unwrap().cleaned);
    assert_eq!(h.backend.state.lock().await.starts, 0);
    h.gateway.close().await.unwrap();
}
#[tokio::test]
async fn assigned_statistics_recover_truncated_event_demand_without_acquire() {
    let mut h = Harness::new(1).await;
    Mock::given(method("POST"))
        .and(path(JIT))
        .respond_with(ResponseTemplate::new(200).set_body_json(jit()))
        .mount(&h.server)
        .await;
    let message = h
        .message_stats(1, vec![], json!({"totalAssignedJobs":1}))
        .await;
    h.controller.handle(42, &message, 40_000).await.unwrap();
    assert_eq!(count(&h.server, "POST", ACQUIRE).await, 0);
    assert_eq!(count(&h.server, "POST", JIT).await, 1);
    assert!(h.store.job(1).await.unwrap().acquired);
    h.gateway.acknowledge(42, 1).await.unwrap();
    h.gateway.close().await.unwrap();
}
#[tokio::test]
async fn actual_execution_is_bound_from_runner_name_not_assumed_acquisition() {
    let mut h = Harness::new(1).await;
    h.normal_registration().await;
    h.deliver(1, vec![available()]).await;
    let started = json!({"messageType":"JobStarted","runnerRequestId":999,"runnerId":1234,"runnerName":"rw-node-1-j1","jobId":"actual-job","workflowRunId":88,"ownerName":"example-org","repositoryName":"actual-repo","jobDisplayName":"actual-name"});
    h.deliver(2, vec![started]).await;
    assert_eq!(h.store.job(1).await.unwrap().actual_request_id, Some(999));
    let actual = h.store.job(1).await.unwrap().metadata;
    assert_eq!(actual.github_job_id, "actual-job");
    assert_eq!(actual.workflow_run_id, 88);
    assert_eq!(actual.repo, "example-org/actual-repo");
    assert_eq!(actual.name, "actual-name");
    h.backend
        .state
        .lock()
        .await
        .processes
        .insert(1, ProcessState::Exited(Some(0)));
    let mut result = complete();
    result["runnerRequestId"] = json!(999);
    h.deliver(3, vec![result]).await;
    assert_eq!(h.store.job(1).await.unwrap().state, State::Completed);
    // JIT material is neither a journal field nor serialized into its pages/WAL.
    for entry in std::fs::read_dir(h.directory.path()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            assert!(
                !String::from_utf8_lossy(&std::fs::read(entry.path()).unwrap())
                    .contains("synthetic-jit-secret")
            );
        }
    }
    h.gateway.close().await.unwrap();
}

#[tokio::test]
async fn idle_timeout_uses_delete_before_stopping_and_keeps_busy_runners() {
    let mut h = Harness::new(1).await;
    h.config.standalone.as_mut().unwrap().idle_seconds = 1;
    h.restart().await;
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
    tokio::time::sleep(Duration::from_millis(1100)).await;
    h.controller.tick(40_000).await.unwrap();
    assert_eq!(
        h.backend.state.lock().await.processes[&1],
        ProcessState::Running
    );
    assert!(!h.store.runner(1).await.unwrap().unwrap().remote_deleted);
    h.controller.tick(41_000).await.unwrap();
    assert!(h.store.runner(1).await.unwrap().unwrap().remote_deleted);
    let state = h.backend.state.lock().await;
    let stopped = state
        .operations
        .iter()
        .position(|op| op == "stop:1")
        .unwrap();
    let measured = state
        .operations
        .iter()
        .position(|op| op == "measure:1")
        .unwrap();
    assert!(stopped < measured);
    drop(state);
    h.gateway.close().await.unwrap();
}

#[tokio::test]
async fn deprecated_template_remains_an_admission_brake_after_restart() {
    let mut h = Harness::new(1).await;
    h.normal_registration().await;
    h.deliver(1, vec![available()]).await;
    h.backend
        .state
        .lock()
        .await
        .processes
        .insert(1, ProcessState::Exited(Some(7)));
    h.controller.tick(40_000).await.unwrap();
    assert!(h.store.runner(1).await.unwrap().unwrap().cleaned);
    h.restart().await;
    assert_eq!(h.controller.capacities(50_000).await.unwrap()[&42], 0);
    let mut second = available();
    second["runnerRequestId"] = json!(102);
    h.deliver(2, vec![second]).await;
    assert_eq!(count(&h.server, "POST", JIT).await, 1);
    h.gateway.close().await.unwrap();
}

#[tokio::test]
async fn template_promotion_updates_pending_installs_and_preserves_running_ones() {
    let mut h = Harness::new(1).await;
    // Rejected acquisition leaves a durable admission and prepared dir.
    Mock::given(method("POST"))
        .and(path(ACQUIRE))
        .respond_with(ResponseTemplate::new(400))
        .up_to_n_times(1)
        .mount(&h.server)
        .await;
    let message = h.message(1, vec![available()]).await;
    assert!(h.controller.handle(42, &message, 40_000).await.is_err());
    assert_eq!(
        h.store.runner(1).await.unwrap().unwrap().template_version,
        "2.337.0"
    );
    h.backend.state.lock().await.template_version = Some("2.338.0".into());
    h.normal_registration().await;
    h.controller.handle(42, &message, 41_000).await.unwrap();
    assert_eq!(
        h.store.runner(1).await.unwrap().unwrap().template_version,
        "2.338.0"
    );
    assert_eq!(
        h.backend.state.lock().await.plans[&1].template_version,
        "2.338.0"
    );
    h.backend.state.lock().await.template_version = Some("2.339.0".into());
    h.controller.tick(42_000).await.unwrap();
    assert_eq!(
        h.store.runner(1).await.unwrap().unwrap().template_version,
        "2.338.0"
    );
    h.gateway.acknowledge(42, 1).await.unwrap();
    h.gateway.close().await.unwrap();
}

#[tokio::test]
async fn session_recreation_during_acquire_preserves_intent_and_resets_delivery() {
    let mut h = Harness::new(1).await;
    Mock::given(method("POST"))
        .and(path(ACQUIRE))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.server)
        .await;
    h.normal_registration().await;
    let message = h.message(1, vec![available()]).await;
    assert!(matches!(
        h.controller.handle(42, &message, 40_000).await,
        Err(runwell_node::Error::SessionReset)
    ));
    assert_eq!(h.store.job(1).await.unwrap().state, State::Admitted);
    assert_eq!(count(&h.server, "DELETE", "/queue/1").await, 0);
    let (_, initial) = h.gateway.next().await.unwrap();
    assert!(initial.message_id.is_none());
    h.controller.handle(42, &initial, 41_000).await.unwrap();
    h.deliver(1, vec![available()]).await;
    assert_eq!(count(&h.server, "POST", JIT).await, 1);
    h.gateway.close().await.unwrap();
}
