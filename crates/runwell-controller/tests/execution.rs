use runwell_controller::execution::ExecutionSource;
use runwell_github::{Auth, RestClient};
use secrecy::SecretString;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test]
async fn resolves_uuid_execution_by_unique_runner_and_vetoes_failed_steps() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/example/project/actions/runs/7"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id":7,"run_attempt":2,"status":"completed"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/repos/example/project/actions/runs/7/attempts/2/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"total_count":2,"jobs":[
            {"id":10,"runner_name":"rw-controller-j1","status":"completed","conclusion":"failure","steps":[{"conclusion":"failure"}]},
            {"id":11,"runner_name":"rw-controller-j2","status":"completed","conclusion":"failure","steps":[{"conclusion":"success"}]}
        ]}))).mount(&server).await;
    let client = RestClient::new(
        server.uri().parse().unwrap(),
        Auth::Pat(SecretString::from("test")),
    )
    .unwrap();
    let code = client
        .execution("example/project", 7, "rw-controller-j1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (code.job_id, code.attempt, code.code_failure),
        (10, 2, true)
    );
    let infra = client
        .execution("example/project", 7, "rw-controller-j2")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (infra.job_id, infra.attempt, infra.code_failure),
        (11, 2, false)
    );
    assert!(
        client
            .execution("example/project", 7, "unknown")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn ambiguous_runner_binding_never_resolves_by_job_display_name() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/example/project/actions/runs/7"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id":7,"run_attempt":1,"status":"completed"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/repos/example/project/actions/runs/7/attempts/1/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"total_count":2,"jobs":[
            {"id":10,"name":"test","runner_name":"same","status":"completed","conclusion":"failure","steps":[]},
            {"id":11,"name":"test","runner_name":"same","status":"completed","conclusion":"failure","steps":[]}
        ]}))).mount(&server).await;
    let client = RestClient::new(
        server.uri().parse().unwrap(),
        Auth::Pat(SecretString::from("test")),
    )
    .unwrap();
    assert!(
        client
            .execution("example/project", 7, "same")
            .await
            .unwrap()
            .is_none()
    );
}
