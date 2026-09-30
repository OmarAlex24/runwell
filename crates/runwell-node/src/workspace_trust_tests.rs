use super::*;
use runwell_store::{NewJob, State};
use serde_json::json;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
fn job() -> Job {
    Job {
        id: 1,
        metadata: NewJob {
            scale_set_id: 1,
            request_id: 2,
            github_job_id: "job".into(),
            workflow_run_id: 3,
            repo: "owner/repo".into(),
            name: "test".into(),
            class: "small".into(),
            reserved_cpu: 1,
            reserved_memory: 1,
        },
        state: State::Completed,
        acquired: true,
        actual_request_id: Some(2),
        outcome: Some("succeeded".into()),
        outcome_at: Some(1),
        started_at: Some(1),
    }
}
#[tokio::test]
async fn trust_uses_authenticated_run_head_and_repository_default_branch() {
    let server = MockServer::start().await;
    let secret = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(secret.path(), "synthetic-token").unwrap();
    let config = GithubConfig {
        config_url: "https://github.com/owner/repo".into(),
        auth: AuthConfig::Pat {
            token_file: secret.path().into(),
        },
    };
    let mut trust = WorkspaceTrust::new(&config).unwrap();
    trust.api = reqwest::Url::parse(&format!("{}/", server.uri())).unwrap();
    Mock::given(method("GET")).and(path("/repos/owner/repo/actions/runs/3")).and(header("authorization", "Bearer synthetic-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":3,"event":"push","head_branch":"main","repository":{"full_name":"owner/repo"},"head_repository":{"full_name":"owner/repo"}}))).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo"))
        .and(header("authorization", "Bearer synthetic-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"full_name":"owner/repo","default_branch":"main"})),
        )
        .mount(&server)
        .await;
    let completion = trust.completion(&job()).await.unwrap();
    assert!(completion.succeeded && completion.same_repository);
    assert_eq!(completion.branch, "main");
    assert_eq!(completion.default_branch, "main");
    assert_eq!(completion.event, "push");
    let mut failed = job();
    failed.outcome = Some("canceled".into());
    assert!(!trust.completion(&failed).await.unwrap().succeeded);
}
#[tokio::test]
async fn missing_or_mismatched_evidence_fails_closed() {
    let server = MockServer::start().await;
    let secret = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(secret.path(), "synthetic-token").unwrap();
    let config = GithubConfig {
        config_url: "https://github.com/owner/repo".into(),
        auth: AuthConfig::Pat {
            token_file: secret.path().into(),
        },
    };
    let mut trust = WorkspaceTrust::new(&config).unwrap();
    trust.api = reqwest::Url::parse(&format!("{}/", server.uri())).unwrap();
    assert!(trust.completion(&job()).await.is_err());
    Mock::given(method("GET")).and(path("/repos/owner/repo/actions/runs/3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":999,"event":"push","head_branch":"main","repository":{"full_name":"attacker/repo"},"head_repository":null}))).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"full_name":"owner/repo","default_branch":"main"})),
        )
        .mount(&server)
        .await;
    assert!(trust.completion(&job()).await.is_err());
}
