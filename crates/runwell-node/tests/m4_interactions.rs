mod support;
use runwell_node::{JobPlan, ProcessState, SliceSpec};
use support::*;
use wiremock::{Mock, ResponseTemplate, matchers::*};

#[tokio::test]
async fn failed_proxy_cleanup_blocks_harvest_and_successful_retry_orders_all_steps() {
    let mut h = Harness::new(1).await;
    h.normal_registration().await;
    h.deliver(1, vec![available()]).await;
    {
        let mut state = h.backend.state.lock().await;
        state.processes.insert(1, ProcessState::Exited(Some(0)));
        state.cleanup_failures.insert(1);
        state.operations.clear();
    }
    let message = h.message(2, vec![complete()]).await;
    assert!(h.controller.handle(42, &message, 40_000).await.is_err());
    {
        let mut state = h.backend.state.lock().await;
        assert_eq!(state.operations, ["stop:1"]);
        state.cleanup_failures.clear();
        state.operations.clear();
    }
    assert_eq!(
        count(&h.server, "DELETE", &format!("{AGENTS}/1234")).await,
        1
    );
    assert!(!h.store.runner(1).await.unwrap().unwrap().cleaned);
    h.controller.tick(41_000).await.unwrap();
    assert_eq!(
        h.backend.state.lock().await.operations,
        ["stop:1", "measure:1", "harvest:1", "cleanup:1"]
    );
    assert!(h.store.runner(1).await.unwrap().unwrap().cleaned);
    h.gateway.close().await.unwrap();
}

async fn orphans(h: &Harness) {
    let mut state = h.backend.state.lock().await;
    state.operations.clear();
    for id in [10, 11] {
        state.plans.insert(
            id,
            JobPlan {
                slice: SliceSpec {
                    job_id: id,
                    memory_high: 1,
                    memory_max: 2,
                    cpu_weight: 100,
                    tasks_max: 64,
                },
                directory: h.directory.path().join(format!("j{id}")),
                template_version: "probe".into(),
            },
        );
    }
}

#[tokio::test]
async fn orphan_proxy_failure_does_not_skip_other_orphans_or_workspace_reconcile() {
    let mut h = Harness::new(1).await;
    orphans(&h).await;
    h.backend.state.lock().await.cleanup_failures.insert(10);
    assert!(h.controller.reconcile().await.is_err());
    let state = h.backend.state.lock().await;
    assert_eq!(
        state.operations,
        ["cleanup:10", "cleanup:11", "reconcile_workspaces"]
    );
    assert_eq!(state.retained_workspaces, [10].into());
    assert!(state.plans.contains_key(&10));
    assert!(!state.plans.contains_key(&11));
    drop(state);
    h.gateway.close().await.unwrap();
}

#[tokio::test]
async fn workspace_failure_does_not_skip_proxy_cleanup() {
    let mut h = Harness::new(1).await;
    orphans(&h).await;
    h.backend.state.lock().await.workspace_reconcile_failure = true;
    assert!(h.controller.reconcile().await.is_err());
    let state = h.backend.state.lock().await;
    assert_eq!(
        state.operations,
        ["cleanup:10", "cleanup:11", "reconcile_workspaces"]
    );
    assert!(state.plans.is_empty());
    drop(state);
    h.gateway.close().await.unwrap();
}

#[tokio::test]
async fn unconfirmed_remote_delete_retains_workspace_while_other_orphans_reconcile() {
    let mut h = Harness::new(1).await;
    orphans(&h).await;
    Mock::given(method("GET"))
        .and(path(AGENTS))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .mount(&h.server)
        .await;
    assert!(h.controller.reconcile().await.is_err());
    let state = h.backend.state.lock().await;
    assert_eq!(state.operations, ["reconcile_workspaces"]);
    assert_eq!(state.retained_workspaces, [10, 11].into());
    drop(state);
    h.gateway.close().await.unwrap();
}
