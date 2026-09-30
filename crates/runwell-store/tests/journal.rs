use runwell_store::*;
async fn journal() -> (tempfile::TempDir, Store, i64) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("state.sqlite").to_str().unwrap())
        .await
        .unwrap();
    let id = store
        .queue(NewJob {
            scale_set_id: 42,
            request_id: 1,
            github_job_id: "job-uuid".into(),
            workflow_run_id: 7,
            repo: "example/repo".into(),
            name: "test".into(),
            class: "small".into(),
            reserved_cpu: 1,
            reserved_memory: 1024,
        })
        .await
        .unwrap();
    (dir, store, id)
}
fn runner(id: i64) -> Runner {
    Runner {
        job_id: id,
        name: "rw-test-j1".into(),
        dir: "/var/lib/runwell/runners/j1".into(),
        unit: "rw-j1.service".into(),
        template_version: "2.337.0".into(),
        agent_id: None,
        exit_code: None,
        remote_deleted: false,
        cleaned: false,
    }
}
#[tokio::test]
async fn journal_enforces_legal_transitions_and_crash_safe_cleanup_order() {
    let (_dir, store, id) = journal().await;
    for illegal in [State::RunnerCreated, State::Running, State::Completed] {
        assert!(store.transition(id, illegal).await.is_err());
    }
    store.transition(id, State::Admitted).await.unwrap();
    store.runner_intent(runner(id)).await.unwrap();
    store.runner_intent(runner(id)).await.unwrap();
    assert!(store.registered(id, 90).await.is_err());
    store.acquired(id).await.unwrap();
    store.registered(id, 90).await.unwrap();
    assert!(store.retarget_template(id, "2.338.0".into()).await.is_err());
    store.registered(id, 90).await.unwrap();
    assert!(store.registered(id, 91).await.is_err());
    store.transition(id, State::Running).await.unwrap();
    store.transition(id, State::Running).await.unwrap();
    assert!(store.transition(id, State::Admitted).await.is_err());
    assert!(store.cleaned(id).await.is_err());
    store
        .record(&JobMeasurement {
            job_id: id,
            cpu_usec: 12,
            oom_kills: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    store
        .record(&JobMeasurement {
            job_id: id,
            cpu_usec: 99,
            ..Default::default()
        })
        .await
        .unwrap();
    let m = store.measurement(id).await.unwrap().unwrap();
    assert_eq!(m.cpu_usec, 12);
    assert!(m.infra_signal);
    store.transition(id, State::Failed).await.unwrap();
    assert!(store.cleaned(id).await.is_err());
    store.remote_deleted(id).await.unwrap();
    store.cleaned(id).await.unwrap();
    assert!(store.transition(id, State::Running).await.is_err());
}
#[tokio::test]
async fn concurrent_duplicate_delivery_and_reopen_preserve_identity() {
    let (dir, store, id) = journal().await;
    let job = store.job(id).await.unwrap().metadata;
    let (a, b) = tokio::join!(store.queue(job.clone()), store.queue(job));
    assert_eq!((a.unwrap(), b.unwrap()), (id, id));
    store.acked(42, 99).await.unwrap();
    let reopened = Store::open(dir.path().join("state.sqlite").to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(reopened.jobs().await.unwrap().len(), 1);
    assert_eq!(reopened.last_acked(42).await.unwrap(), Some(99));
    let options =
        sqlx::sqlite::SqliteConnectOptions::new().filename(dir.path().join("state.sqlite"));
    let connection = sqlx::SqlitePool::connect_with(options).await.unwrap();
    let wal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&connection)
        .await
        .unwrap();
    assert_eq!(wal, "wal");
}
