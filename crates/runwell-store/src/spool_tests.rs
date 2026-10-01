use super::*;

async fn spool() -> (tempfile::TempDir, Store, NodeLease) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("spool.sqlite").to_str().unwrap())
        .await
        .unwrap();
    let id = store
        .queue(NewJob {
            scale_set_id: 1,
            request_id: 1,
            github_job_id: String::new(),
            workflow_run_id: 1,
            repo: "example/repo".into(),
            name: "large payload".repeat(2048),
            class: "small".into(),
            reserved_cpu: 1,
            reserved_memory: 1024,
        })
        .await
        .unwrap();
    let lease = NodeLease {
        job: store.job(id).await.unwrap(),
        attempt: 1,
        phase: 4,
        plan: Some("large plan".repeat(2048)),
        measurement: Some(JobMeasurement {
            job_id: id,
            cpu_usec: 123,
            ..Default::default()
        }),
        started_at_ms: Some(1000),
        heartbeat_at_ms: None,
    };
    (dir, store, lease)
}

#[tokio::test]
async fn history_is_compact_and_routine_queries_touch_only_active_attempts() {
    let (_dir, store, mut lease) = spool().await;
    for id in 1..=256 {
        lease.job.id = id;
        lease.phase = 4;
        store.lease(lease.clone()).await.unwrap();
        lease.phase = 6;
        store.lease(lease.clone()).await.unwrap();
    }
    let (count, payload_bytes): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*),COALESCE(SUM(LENGTH(payload)),0) FROM node_leases")
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert_eq!((count, payload_bytes), (256, 0));
    let tombstone = store.lease_record(1).await.unwrap().unwrap();
    assert_eq!(tombstone.phase, 6);
    assert_eq!(tombstone.measurement().unwrap().unwrap().cpu_usec, 123);
    assert!(store.node_lease(1, 1).await.unwrap().is_none());
    // Historical payload corruption must not be fetched/deserialized by active
    // reports or per-attempt lookups. No routine code scans completed history.
    sqlx::query("UPDATE node_leases SET payload='invalid historical payload'")
        .execute(&store.pool)
        .await
        .unwrap();
    lease.job.id = 257;
    lease.phase = 4;
    store.lease(lease.clone()).await.unwrap();
    assert_eq!(store.active_leases().await.unwrap().len(), 1);
    assert_eq!(store.node_lease(257, 1).await.unwrap().unwrap().job.id, 257);
    assert!(store.node_lease(257, 2).await.unwrap().is_none());
    let active_plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT payload FROM node_leases WHERE phase<6 ORDER BY job_id",
    )
    .fetch_all(&store.pool)
    .await
    .unwrap();
    assert!(
        active_plan
            .iter()
            .any(|(_, _, _, p)| p.contains("node_leases_active"))
    );
    let lookup_plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT payload FROM node_leases WHERE job_id=257 AND attempt=1 AND phase<6"
    ).fetch_all(&store.pool).await.unwrap();
    assert!(
        lookup_plan
            .iter()
            .any(|(_, _, _, p)| p.contains("SEARCH") && p.contains("PRIMARY KEY"))
    );
    lease.job.id = 1;
    assert!(matches!(
        store.lease(lease.clone()).await,
        Err(Error::Transition)
    ));
    lease.attempt = 2;
    assert!(matches!(store.lease(lease).await, Err(Error::Transition)));
}

#[tokio::test]
async fn heartbeat_and_start_epochs_survive_stale_lifecycle_writes_and_reopen() {
    let (dir, store, mut lease) = spool().await;
    store.lease(lease.clone()).await.unwrap();
    store.runner_heartbeat(lease.job.id, 1, 1500).await.unwrap();
    store.runner_heartbeat(lease.job.id, 1, 1200).await.unwrap();
    store.runner_heartbeat(lease.job.id, 2, 9000).await.unwrap();
    lease.phase = 5;
    lease.started_at_ms = Some(5000);
    store.lease(lease.clone()).await.unwrap();
    let reopened = Store::open(dir.path().join("spool.sqlite").to_str().unwrap())
        .await
        .unwrap();
    let recovered = reopened.node_lease(lease.job.id, 1).await.unwrap().unwrap();
    assert_eq!(recovered.started_at_ms, Some(1000));
    assert_eq!(recovered.heartbeat_at_ms, Some(1500));
    lease.phase = 6;
    reopened.lease(lease.clone()).await.unwrap();
    reopened
        .runner_heartbeat(lease.job.id, 1, 9000)
        .await
        .unwrap();
    let tombstone = reopened.lease_record(lease.job.id).await.unwrap().unwrap();
    assert_eq!(tombstone.heartbeat_at_ms, Some(1500));
    assert!(reopened.active_leases().await.unwrap().is_empty());
}

#[tokio::test]
async fn spool_migration_compacts_existing_cleaned_payloads() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql("CREATE TABLE node_leases(job_id INTEGER PRIMARY KEY,attempt INTEGER,payload TEXT NOT NULL); CREATE TABLE placements(job_id INTEGER PRIMARY KEY);")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO node_leases VALUES(1,1,?)").bind(r#"{"phase":6,"job":{"name":"old payload"},"plan":"large plan","measurement":{"job_id":1}}"#)
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO node_leases VALUES(2,1,?)")
        .bind(r#"{"phase":4,"job":{"name":"still running"},"measurement":null}"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../migrations/0203_node_spool.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let rows: Vec<(i64, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT job_id,payload,measurement FROM node_leases ORDER BY job_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows[0], (1, None, Some(r#"{"job_id":1}"#.into())));
    assert!(rows[1].1.is_some());
    assert!(rows[1].2.is_none());
}
