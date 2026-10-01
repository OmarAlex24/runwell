use runwell_github::{Auth, RestClient};
use runwell_retry::*;
use runwell_store::{RetryStatus, Store};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};
struct Fixture {
    _dir: tempfile::TempDir,
    server: MockServer,
    db: Store,
    api: RestClient,
    c: Classifier,
}
impl Fixture {
    async fn new(conclusions: &[&str], post_status: u16, post_count: u64) -> Self {
        let server = MockServer::start().await;
        let jobs: Vec<_> = conclusions
            .iter()
            .enumerate()
            .map(|(i, c)| json!({"id":i+1,"status":"completed","conclusion":c}))
            .collect();
        Mock::given(method("GET"))
            .and(path("/repos/a/repo/actions/runs/10"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id":10,"run_attempt":1,"status":"completed"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/a/repo/actions/runs/10/attempts/1/jobs"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"total_count":jobs.len(),"jobs":jobs})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/a/repo/actions/runs/10/rerun-failed-jobs"))
            .respond_with(ResponseTemplate::new(post_status))
            .expect(post_count)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(&format!("sqlite://{}", dir.path().join("db").display()))
            .await
            .unwrap();
        let api =
            RestClient::new(server.uri().parse().unwrap(), Auth::Pat("secret".into())).unwrap();
        Self {
            _dir: dir,
            server,
            db,
            api,
            c: Classifier::builtin().unwrap(),
        }
    }
    async fn send(&self, request: &RetryRequest) -> Result<Outcome, Error> {
        retry(
            &RetryPolicy {
                enabled: true,
                daily_cap: 10,
            },
            &self.c,
            &self.db,
            &self.api,
            request,
            86_400,
        )
        .await
    }
}
fn request() -> RetryRequest {
    RetryRequest {
        repo: "A/Repo".into(),
        run_id: 10,
        attempt: 1,
        evidence: [(
            1,
            FailureEvidence {
                conclusion: "failure".into(),
                signals: [Signal::NodeLost].into(),
                annotations_and_tail: String::new(),
            },
        )]
        .into(),
    }
}
#[tokio::test]
async fn accepted_retry_is_at_most_once_across_concurrent_handlers_and_restart() {
    let f = Fixture::new(&["failure", "success"], 201, 1).await;
    let request = request();
    let (a, b) = tokio::join!(f.send(&request), f.send(&request));
    let results = [a.unwrap(), b.unwrap()];
    assert!(results.contains(&Outcome::Accepted));
    assert!(results.contains(&Outcome::Suppressed));
    let reopened = Store::open(&format!("sqlite://{}", f._dir.path().join("db").display()))
        .await
        .unwrap();
    assert_eq!(
        retry(
            &RetryPolicy {
                enabled: true,
                daily_cap: 10
            },
            &f.c,
            &reopened,
            &f.api,
            &request,
            2 * 86_400
        )
        .await
        .unwrap(),
        Outcome::Suppressed
    );
    assert_eq!(
        reopened
            .retry_record("a/repo", 10, 1)
            .await
            .unwrap()
            .unwrap()
            .status,
        RetryStatus::Accepted
    );
}
#[tokio::test]
async fn ambiguous_and_rejected_requests_are_never_resent() {
    for (http, status) in [(500, RetryStatus::Ambiguous), (403, RetryStatus::Rejected)] {
        let f = Fixture::new(&["failure"], http, 1).await;
        assert!(f.send(&request()).await.is_err());
        assert_eq!(f.send(&request()).await.unwrap(), Outcome::Suppressed);
        assert_eq!(
            f.db.retry_record("a/repo", 10, 1)
                .await
                .unwrap()
                .unwrap()
                .status,
            status
        );
    }
}
#[tokio::test]
async fn mixed_or_unknown_failures_and_missing_evidence_prevent_run_wide_rerun() {
    let f = Fixture::new(&["failure", "failure"], 201, 0).await;
    let mut req = request();
    assert_eq!(f.send(&req).await.unwrap(), Outcome::Ineligible);
    req.evidence.insert(
        2,
        FailureEvidence {
            conclusion: "failure".into(),
            signals: [Signal::RunnerCrash].into(),
            annotations_and_tail: "tests failed".into(),
        },
    );
    assert_eq!(f.send(&req).await.unwrap(), Outcome::Ineligible);
    req.evidence.get_mut(&2).unwrap().signals.clear();
    req.evidence
        .get_mut(&2)
        .unwrap()
        .annotations_and_tail
        .clear();
    assert_eq!(f.send(&req).await.unwrap(), Outcome::Ineligible);
    assert!(f.db.retry_record("a/repo", 10, 1).await.unwrap().is_none());
}
#[tokio::test]
async fn off_switch_and_zero_budget_make_no_calls_and_stale_attempts_do_not_send() {
    let f = Fixture::new(&["failure"], 201, 0).await;
    for policy in [
        RetryPolicy::default(),
        RetryPolicy {
            enabled: true,
            daily_cap: 0,
        },
    ] {
        assert_eq!(
            retry(&policy, &f.c, &f.db, &f.api, &request(), 0)
                .await
                .unwrap(),
            Outcome::Disabled
        );
    }
    assert!(f.server.received_requests().await.unwrap().is_empty());
    let mut req = request();
    req.attempt = 2;
    assert_eq!(f.send(&req).await.unwrap(), Outcome::Stale);
}
#[tokio::test]
async fn multi_failure_run_charges_the_daily_cap_per_job() {
    let f = Fixture::new(&["failure", "failure"], 201, 0).await;
    let mut req = request();
    req.evidence.insert(2, req.evidence[&1].clone());
    assert_eq!(
        retry(
            &RetryPolicy {
                enabled: true,
                daily_cap: 1
            },
            &f.c,
            &f.db,
            &f.api,
            &req,
            0
        )
        .await
        .unwrap(),
        Outcome::Suppressed
    );
}

#[tokio::test]
async fn pytest_red_network_failure_never_claims_or_posts_a_retry() {
    let f = Fixture::new(&["failure"], 201, 0).await;
    let mut req = request();
    req.evidence.insert(1, FailureEvidence {
        conclusion: "failure".into(), signals: Default::default(),
        annotations_and_tail: "FAILED tests/test_network.py::test_connection - OSError: [Errno 101] Network is unreachable".into(),
    });
    assert_eq!(f.send(&req).await.unwrap(), Outcome::Ineligible);
    assert!(f.db.retry_record("a/repo", 10, 1).await.unwrap().is_none());
}
