use runwell_scheduler::{Criticality, DurationEstimate, HistoryKey};
use runwell_store::{CompletedJob, RetryClaim, RetryStatus, Store};
use std::collections::BTreeMap;
fn completion(id: i64, duration: u64) -> CompletedJob {
    CompletedJob {
        key: HistoryKey {
            repo: "a/repo".into(),
            workflow_job: "ci/test".into(),
            class: "small".into(),
        },
        github_job_id: id,
        run_id: id,
        attempt: 1,
        completed_at: id,
        duration_ms: duration,
        queue_ms: 900_000,
        conclusion: "success".into(),
        criticality: Criticality {
            depth: 2,
            fan_out: 3,
        },
    }
}
async fn store() -> (tempfile::TempDir, String, Store) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("db").display());
    let store = Store::open(&url).await.unwrap();
    (dir, url, store)
}
#[tokio::test]
async fn window_is_incremental_durable_and_deduplicated_with_cold_start() {
    let (_dir, url, db) = store().await;
    let default = DurationEstimate {
        p50_seconds: 30.0,
        p90_seconds: 60.0,
        samples: 0,
        criticality: Criticality::default(),
    };
    let cold = db
        .duration_history([("small".into(), default)].into())
        .await
        .unwrap();
    assert_eq!(cold.estimate(&completion(1, 1).key), Some(default));
    for id in 1..=200 {
        db.record_completion(completion(id, id as u64 * 1000))
            .await
            .unwrap();
    }
    db.record_completion(completion(200, 999_999_999))
        .await
        .unwrap();
    let mut failed = completion(201, 999_999_999);
    failed.conclusion = "failure".into();
    db.record_completion(failed).await.unwrap();
    let fresh = Store::open(&url)
        .await
        .unwrap()
        .duration_history(BTreeMap::new())
        .await
        .unwrap();
    let estimate = fresh.estimate(&completion(1, 1).key).unwrap();
    assert_eq!(estimate.samples, 128);
    assert_eq!(estimate.p50_seconds, 136.0);
    assert_eq!(estimate.p90_seconds, 188.0);
    assert_eq!(
        estimate.criticality,
        Criticality {
            depth: 2,
            fan_out: 3
        }
    );
    let mut other = completion(202, 5000);
    other.key.repo = "other/repo".into();
    db.record_completion(other).await.unwrap();
    assert_eq!(
        db.duration_history(BTreeMap::new())
            .await
            .unwrap()
            .jobs
            .len(),
        2
    );
}
fn claim(run: i64, day: i64) -> RetryClaim {
    RetryClaim {
        repo: "a/repo".into(),
        run_id: run,
        attempt: 1,
        job_ids: vec![run],
        utc_day: day,
        daily_cap: 2,
    }
}
#[tokio::test]
async fn concurrent_claims_share_a_durable_daily_budget_and_no_refunds() {
    let (_dir, url, db) = store().await;
    let mut tasks = Vec::new();
    for run in 1..=20 {
        let db = db.clone();
        tasks.push(tokio::spawn(async move {
            db.claim_retry(claim(run, 10)).await.unwrap()
        }));
    }
    let mut accepted = 0;
    for task in tasks {
        accepted += usize::from(task.await.unwrap());
    }
    assert_eq!(accepted, 2);
    let db = Store::open(&url).await.unwrap();
    assert!(!db.claim_retry(claim(21, 10)).await.unwrap());
    assert!(db.claim_retry(claim(21, 11)).await.unwrap());
    db.finish_retry("a/repo", 21, 1, RetryStatus::Ambiguous)
        .await
        .unwrap();
    assert!(!db.claim_retry(claim(21, 12)).await.unwrap());
    let mut chain = claim(21, 12);
    chain.attempt = 2;
    chain.job_ids = vec![200];
    assert!(!db.claim_retry(chain).await.unwrap());
    assert_eq!(
        db.retry_record("a/repo", 21, 1)
            .await
            .unwrap()
            .unwrap()
            .status,
        RetryStatus::Ambiguous
    );
}
#[tokio::test]
async fn multi_job_claim_is_atomic_and_utc_day_changes_budget_only() {
    let (_dir, _url, db) = store().await;
    let mut bulk = claim(1, 0);
    bulk.job_ids = vec![1, 2, 3];
    assert!(!db.claim_retry(bulk).await.unwrap());
    assert!(db.retry_record("a/repo", 1, 1).await.unwrap().is_none());
    let mut bulk = claim(1, 0);
    bulk.job_ids = vec![1, 2];
    assert!(db.claim_retry(bulk.clone()).await.unwrap());
    assert!(!db.claim_retry(bulk).await.unwrap());
    let mut disabled = claim(5, 1);
    disabled.daily_cap = 0;
    assert!(!db.claim_retry(disabled).await.unwrap());
}
