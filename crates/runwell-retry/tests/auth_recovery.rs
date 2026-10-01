use runwell_github::{Auth, RestClient};
use runwell_retry::{
    Classifier, FailureEvidence, Outcome, RetryPolicy, RetryRequest, Signal, retry,
};
use runwell_store::{RetryStatus, Store};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{header, method, path},
};

#[tokio::test]
async fn auth_recovery_preserves_one_claim_and_retains_ambiguous_outcomes() {
    for (response, expected) in [(201, RetryStatus::Accepted), (500, RetryStatus::Ambiguous)] {
        let server = MockServer::start().await;
        let exchanges = Arc::new(AtomicUsize::new(0));
        let count = exchanges.clone();
        Mock::given(method("POST"))
            .and(path("/app/installations/99/access_tokens"))
            .respond_with(move |_: &Request| {
                let n = count.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(201).set_body_json(
                    json!({"token":format!("token-{n}"),"expires_at":"2099-01-01T00:00:00Z"}),
                )
            })
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/a/repo/actions/runs/10"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id":10,"run_attempt":1,"status":"completed"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/repos/a/repo/actions/runs/10/attempts/1/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"total_count":1,"jobs":[{"id":1,"status":"completed","conclusion":"failure"}]}))).mount(&server).await;
        for (token, status) in [("token-0", 401), ("token-1", response)] {
            Mock::given(method("POST"))
                .and(path("/repos/a/repo/actions/runs/10/rerun-failed-jobs"))
                .and(header("authorization", format!("Bearer {token}")))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
        }
        let api = RestClient::new(
            server.uri().parse().unwrap(),
            Auth::App {
                app_id: 5,
                installation_id: 99,
                private_key: include_str!("../../runwell-scaleset/tests/fixtures/app-test-key.pem")
                    .into(),
            },
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(&format!("sqlite://{}", dir.path().join("db").display()))
            .await
            .unwrap();
        let policy = RetryPolicy {
            enabled: true,
            daily_cap: 1,
        };
        let classifier = Classifier::builtin().unwrap();
        let request = RetryRequest {
            repo: "a/repo".into(),
            run_id: 10,
            attempt: 1,
            evidence: [(
                1,
                FailureEvidence {
                    conclusion: "failure".into(),
                    signals: [Signal::OomKill].into(),
                    annotations_and_tail: String::new(),
                },
            )]
            .into(),
        };
        let result = retry(&policy, &classifier, &db, &api, &request, 0).await;
        if response == 201 {
            assert_eq!(result.unwrap(), Outcome::Accepted);
        } else {
            assert!(result.is_err());
        }
        let record = db.retry_record("a/repo", 10, 1).await.unwrap().unwrap();
        assert_eq!(record.status, expected);
        assert_eq!(record.job_count, 1);
        assert_eq!(
            retry(&policy, &classifier, &db, &api, &request, 0)
                .await
                .unwrap(),
            Outcome::Suppressed
        );
        assert_eq!(exchanges.load(Ordering::SeqCst), 2);
    }
}
