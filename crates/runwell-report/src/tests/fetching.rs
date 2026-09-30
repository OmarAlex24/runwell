use super::time;
use crate::{cache::Cache, fetch::Client, trace_build};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param},
};

#[tokio::test]
async fn pagination_backoff_and_cache_use_raw_pages() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("GET"))
        .and(path("/pages"))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"jobs":vec![json!({"id":1});100]}))
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("retry-after", "1"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    Mock::given(method("GET"))
        .and(path("/pages"))
        .and(query_param("page", "2"))
        .respond_with(
            move |_: &wiremock::Request| match calls.fetch_add(1, Ordering::SeqCst) {
                0 => ResponseTemplate::new(429).insert_header("retry-after", "0"),
                1 => ResponseTemplate::new(403).insert_header("retry-after", "0"),
                _ => ResponseTemplate::new(200).set_body_json(json!({"jobs":[{"id":2}]})),
            },
        )
        .expect(3)
        .mount(&server)
        .await;
    let mut client =
        Client::new(server.uri(), "synthetic-token".into(), dir.path().into()).unwrap();
    let start = tokio::time::Instant::now();
    let values = client.pages("/pages", Some("jobs")).await.unwrap();
    assert!(start.elapsed().as_millis() >= 900);
    assert_eq!(values.len(), 101);
    assert_eq!(client.pages("/pages", Some("jobs")).await.unwrap(), values);
    server.verify().await;
}

#[test]
fn cache_checks_key_and_corrupt_entries_are_misses() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().into()).unwrap();
    cache.put("page", "{\"jobs\":[]}".into()).unwrap();
    assert_eq!(
        cache.get("page", false).unwrap(),
        Some("{\"jobs\":[]}".into())
    );
    assert_eq!(cache.get("other", false).unwrap(), None);
}

#[test]
fn api_job_conversion_round_trips_portable_trace() {
    let run: trace_build::Run =
        serde_json::from_value(json!({"id":1,"name":"CI","created_at":time(0),
        "conclusion":"success","event":"pull_request","head_sha":"synthetic-sha"}))
        .unwrap();
    let j=trace_build::job("acme/app",&run,json!({"id":10,"name":"test","runner_name":"runner-1",
        "created_at":time(5),"started_at":time(10),"completed_at":time(20),"conclusion":"success",
        "steps":[{"name":"Test","started_at":time(10),"completed_at":time(20),"conclusion":"success"}]})).unwrap();
    let mut bytes = Vec::new();
    runwell_trace::write_jsonl(&mut bytes, std::slice::from_ref(&j)).unwrap();
    assert_eq!(
        runwell_trace::read_jsonl(bytes.as_slice()).unwrap(),
        vec![j]
    );
}

#[tokio::test]
async fn collection_preserves_attempt_metadata_dependencies_and_failure_evidence() {
    use base64::Engine;
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let run = |attempt, conclusion: &str| {
        json!({
            "id":1,"name":"CI","created_at":time(0),"conclusion":conclusion,
            "event":"pull_request","head_sha":"synthetic-sha","head_branch":"feature",
            "path":".github/workflows/ci.yml","run_attempt":attempt,
        })
    };
    Mock::given(path("/repos/acme/app/actions/runs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count":1,"workflow_runs":[run(2,"success")]
        })))
        .mount(&server)
        .await;
    for (attempt, conclusion) in [(1, "failure"), (2, "success")] {
        Mock::given(path(format!(
            "/repos/acme/app/actions/runs/1/attempts/{attempt}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(run(attempt, conclusion)))
        .expect(1)
        .mount(&server)
        .await;
        let job = json!({"id":attempt*10,"name":"Tests","run_attempt":attempt,
            "created_at":time(0),"started_at":time(10),"completed_at":time(20),
            "runner_name":"runner-1","conclusion":conclusion,
            "check_run_url":format!("{}/repos/acme/app/check-runs/{attempt}",server.uri()),
            "steps":[{"name":"Run tests","started_at":time(10),"completed_at":time(20),"conclusion":conclusion}]
        });
        Mock::given(path(format!(
            "/repos/acme/app/actions/runs/1/attempts/{attempt}/jobs"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"jobs":[job]})))
        .expect(1)
        .mount(&server)
        .await;
    }
    Mock::given(path("/repos/acme/app/check-runs/1/annotations"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"message":"runner lost communication with the server"}])),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/repos/acme/app/actions/jobs/10/logs"))
        .respond_with(ResponseTemplate::new(200).set_body_string("synthetic job log"))
        .expect(1)
        .mount(&server)
        .await;
    let yaml = "jobs:\n  test:\n    name: Tests\n    timeout-minutes: 10\n";
    let content = base64::engine::general_purpose::STANDARD.encode(yaml);
    Mock::given(path("/repos/acme/app/contents/.github/workflows/ci.yml"))
        .and(query_param("ref", "synthetic-sha"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"content":content})))
        .expect(1)
        .mount(&server)
        .await;
    let mut client =
        Client::new(server.uri(), "synthetic-token".into(), dir.path().into()).unwrap();
    let result = crate::fetch::collect(&mut client, &["acme/app".into()], time(0), time(100), 1)
        .await
        .unwrap();
    assert!(result.warnings.is_empty());
    assert_eq!(result.jobs.len(), 2);
    assert_eq!(result.jobs[0].run_conclusion.as_deref(), Some("failure"));
    assert_eq!(result.jobs[1].run_conclusion.as_deref(), Some("success"));
    assert_eq!(result.jobs[0].needs, Some(Vec::new()));
    assert_eq!(result.jobs[0].timeout_minutes, Some(10.0));
    assert_eq!(result.jobs[0].annotations.len(), 1);
    assert_eq!(
        result.jobs[0].log_excerpt.as_deref(),
        Some("synthetic job log")
    );
    server.verify().await;
}

#[tokio::test]
async fn permission_denial_is_not_retried_as_a_rate_limit() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(path("/denied"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({"message":"Resource not accessible by integration"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut client =
        Client::new(server.uri(), "synthetic-token".into(), dir.path().into()).unwrap();
    assert!(matches!(
        client.raw("/denied", false).await,
        Err(crate::Error::Http(403))
    ));
}

#[tokio::test]
async fn run_searches_over_the_github_cap_are_split_before_pagination() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let broad = format!("{}..{}", time(0), time(100));
    let full = broad.clone();
    Mock::given(path("/repos/acme/app/actions/runs"))
        .respond_with(move |request: &wiremock::Request| {
            let is_broad = request
                .url
                .query_pairs()
                .any(|(k, v)| k == "created" && v == full);
            ResponseTemplate::new(200).set_body_json(json!({
                "total_count":if is_broad {2001} else {0},"workflow_runs":[]
            }))
        })
        .expect(3)
        .mount(&server)
        .await;
    let mut client =
        Client::new(server.uri(), "synthetic-token".into(), dir.path().into()).unwrap();
    let result = crate::fetch::collect(&mut client, &["acme/app".into()], time(0), time(100), 0)
        .await
        .unwrap();
    assert!(result.jobs.is_empty());
    let received = server.received_requests().await.unwrap();
    let ranges: std::collections::BTreeSet<_> = received
        .iter()
        .flat_map(|r| r.url.query_pairs())
        .filter(|(k, _)| k == "created")
        .map(|(_, v)| v.into_owned())
        .collect();
    assert_eq!(ranges.len(), 3);
    server.verify().await;
}
