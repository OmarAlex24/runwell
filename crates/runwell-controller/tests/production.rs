mod production_support;
mod support;
use production_support::*;
use runwell_controller::{Hooks, monitoring::serve_metrics};
use runwell_metrics::AlertKind;
use runwell_scheduler::{Criticality, HistoryKey};
use runwell_store::{CompletedJob, FailureEvent, SchedulingContext};
use runwell_transport::Clock;
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::Ordering},
};
use support::Harness;

#[tokio::test]
async fn two_nodes_two_repositories_use_history_fairness_and_retry_only_infra_once() {
    let mut h = Harness::new().await;
    h.config.controller.production.retry_enabled = true;
    let remote = Arc::new(Remote::default());
    let telemetry = make_telemetry(&h, remote.clone());
    wire(&mut h, telemetry.clone());
    let mut jobs = BTreeMap::new();
    // Deliberately enqueue slow, short, gate: FIFO and shortest-only both fail.
    for (repo_index, repo) in ["example/alpha", "example/beta"].into_iter().enumerate() {
        for (index, (name, seconds, depth)) in [("slow", 40, 0), ("short", 2, 0), ("gate", 60, 3)]
            .into_iter()
            .enumerate()
        {
            let request = (repo_index * 10 + index + 1) as i64;
            let id = h
                .store
                .queue(runwell_store::NewJob {
                    scale_set_id: 42,
                    request_id: request,
                    github_job_id: format!("uuid-{request}"),
                    workflow_run_id: repo_index as i64 + 1,
                    repo: repo.into(),
                    name: name.into(),
                    class: "runwell-small".into(),
                    reserved_cpu: 1,
                    reserved_memory: 2147483648,
                })
                .await
                .unwrap();
            let key = HistoryKey {
                repo: repo.into(),
                workflow_job: name.into(),
                class: "runwell-small".into(),
            };
            h.store
                .record_completion(CompletedJob {
                    key,
                    github_job_id: 1000 + request,
                    run_id: 100,
                    attempt: 1,
                    completed_at: h.clock.now_ms() / 1000 - 1,
                    duration_ms: seconds * 1000,
                    queue_ms: 0,
                    conclusion: "success".into(),
                    criticality: Criticality {
                        depth,
                        fan_out: depth,
                    },
                })
                .await
                .unwrap();
            h.store
                .set_scheduling_context(
                    id,
                    SchedulingContext {
                        workflow_job: name.into(),
                        ready_at_ms: h.clock.now_ms(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            jobs.insert((repo_index, name), id);
        }
    }
    for (wave, name) in ["gate", "short", "slow"].into_iter().enumerate() {
        h.tick().await;
        let running: Vec<_> = h
            .store
            .jobs()
            .await
            .unwrap()
            .into_iter()
            .filter(|j| j.state == runwell_store::State::Running)
            .collect();
        assert_eq!(running.len(), 2, "both nodes must be filled");
        assert!(
            running.iter().all(|j| j.metadata.name == name),
            "critical path, then learned short job"
        );
        assert_eq!(
            running
                .iter()
                .map(|j| &j.metadata.repo)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            2,
            "one dispatch per repo per wave"
        );
        for (repo, _) in ["example/alpha", "example/beta"].iter().enumerate() {
            let id = jobs[&(repo, name)];
            let infra = wave == 0 && repo == 0;
            let code = wave == 0 && repo == 1;
            finish(&mut h, &remote, id, infra, code).await;
        }
        h.tick().await;
        telemetry.tick().await.unwrap();
    }
    h.tick().await;
    telemetry.tick().await.unwrap();
    assert_eq!(remote.posts.lock().await.as_slice(), &["example/alpha"]);
    assert_eq!(h.api.creates.load(Ordering::SeqCst), 6);
    let infra = jobs[&(0, "gate")];
    // Outbox redelivery and a new telemetry instance cannot make another POST.
    telemetry
        .failure(&FailureEvent {
            job_id: infra,
            attempt: 1,
            reason: "oom".into(),
        })
        .await
        .unwrap();
    let recovered = make_telemetry(&h, remote.clone());
    recovered.tick().await.unwrap();
    assert_eq!(remote.posts.lock().await.len(), 1);
    let key = HistoryKey {
        repo: "example/alpha".into(),
        workflow_job: "short".into(),
        class: "runwell-small".into(),
    };
    assert_eq!(
        h.store
            .duration_estimate(&key)
            .await
            .unwrap()
            .unwrap()
            .samples,
        2
    );
    assert!(
        telemetry
            .alerts()
            .await
            .unwrap()
            .iter()
            .any(|a| a.kind == AlertKind::InfraFailureRate)
    );
    assert!(h.api.runners.lock().await.is_empty());
    for host in &h.hosts {
        let state = host.state.lock().await;
        assert!(
            state.plans.is_empty()
                && state.processes.is_empty()
                && state.mounts.is_empty()
                && state.containers.is_empty()
        );
    }
    for store in &h.stores {
        assert!(store.active_leases().await.unwrap().is_empty());
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let server = tokio::spawn(serve_metrics(listener, telemetry.metrics(), stop.clone()));
    let response = reqwest::get(format!("http://{address}/metrics"))
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("openmetrics")
    );
    let metrics = response.text().await.unwrap();
    assert!(
        metrics.contains("runwell_retries_total{class=\"runwell-small\"} 1"),
        "{metrics}"
    );
    assert!(
        metrics.contains("runwell_queue_seconds_count")
            && metrics.contains("runwell_node_headroom_cpu_slots")
    );
    stop.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn admission_reply_loss_restores_proposed_fairness_without_duplicate_charge() {
    let mut h = Harness::new().await;
    let telemetry = make_telemetry(&h, Arc::new(Remote::default()));
    wire(&mut h, telemetry);
    let id = h.queue(1).await;
    h.store
        .set_scheduling_context(
            id,
            SchedulingContext {
                workflow_job: "test".into(),
                ready_at_ms: h.clock.now_ms(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    h.links[0].lose_admit.store(true, Ordering::SeqCst);
    assert!(h.controller.tick(h.clock.now_ms() as u64).await.is_err());
    assert_eq!(h.store.fairness().await.unwrap(), Default::default());
    assert_eq!(h.store.pending_dispatches().await.unwrap(), vec![id]);
    h.links[0].lose_admit.store(false, Ordering::SeqCst);
    let telemetry = make_telemetry(&h, Arc::new(Remote::default()));
    wire(&mut h, telemetry);
    h.tick().await;
    let accepted = h.store.fairness().await.unwrap();
    assert!(!accepted.repositories.is_empty());
    h.tick().await;
    assert_eq!(h.store.fairness().await.unwrap(), accepted);
    assert_eq!(h.api.creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn mixed_infra_and_test_run_is_not_retried_but_both_outcomes_are_observed() {
    let mut h = Harness::new().await;
    h.config.controller.production.retry_enabled = true;
    let remote = Arc::new(Remote::default());
    let telemetry = make_telemetry(&h, remote.clone());
    wire(&mut h, telemetry.clone());
    let ids = [h.queue(1).await, h.queue(2).await, h.queue(3).await];
    h.tick().await;
    finish(&mut h, &remote, ids[0], true, false).await;
    finish(&mut h, &remote, ids[1], false, true).await;
    h.tick().await;
    h.tick().await;
    finish(&mut h, &remote, ids[2], false, false).await;
    h.tick().await;
    telemetry.tick().await.unwrap();
    telemetry.tick().await.unwrap();
    assert!(remote.posts.lock().await.is_empty());
    let observations = h.store.observations(0).await.unwrap();
    assert_eq!(observations.len(), 3);
    assert!(observations.iter().all(|o| o.processed && o.counted));
    assert_eq!(observations.iter().filter(|o| o.infra).count(), 1);
    assert!(
        telemetry
            .alerts()
            .await
            .unwrap()
            .iter()
            .any(|a| a.kind == AlertKind::InfraFailureRate)
    );
}
