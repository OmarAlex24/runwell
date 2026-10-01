use runwell_github::{Auth, RestClient};
use secrecy::SecretString;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};
fn client(server: &MockServer) -> RestClient {
    RestClient::new(
        server.uri().parse().unwrap(),
        Auth::Pat(SecretString::from("secret")),
    )
    .unwrap()
}
#[tokio::test]
async fn rerun_failed_uses_pat_headers_and_one_post_even_on_server_failure() {
    for status in [201, 401, 403, 429, 500, 502] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/a/repo/actions/runs/42/rerun-failed-jobs"))
            .and(header("authorization", "Bearer secret"))
            .and(header("accept", "application/vnd.github+json"))
            .and(header("x-github-api-version", "2026-03-10"))
            .and(body_json(json!({"enable_debug_logging":false})))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
        let result = client(&server).rerun_failed_jobs("a/repo", 42).await;
        assert_eq!(result.is_ok(), status == 201);
        if let Err(error) = result {
            assert_eq!(error.ambiguous(), status >= 500);
        }
    }
}
#[tokio::test]
async fn app_exchange_is_single_flight_and_reruns_use_installation_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/app/installations/99/access_tokens"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"token":"install","expires_at":"2099-01-01T00:00:00Z"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/a/repo/actions/runs/42/rerun-failed-jobs"))
        .and(header("authorization", "Bearer install"))
        .respond_with(ResponseTemplate::new(201))
        .expect(2)
        .mount(&server)
        .await;
    let app = RestClient::new(
        server.uri().parse().unwrap(),
        Auth::App {
            app_id: 5,
            installation_id: 99,
            private_key: SecretString::from(include_str!(
                "../../runwell-scaleset/tests/fixtures/app-test-key.pem"
            )),
        },
    )
    .unwrap();
    let (a, b) = tokio::join!(
        app.rerun_failed_jobs("a/repo", 42),
        app.rerun_failed_jobs("a/repo", 42)
    );
    a.unwrap();
    b.unwrap();
    let requests = server.received_requests().await.unwrap();
    let auth = requests[0]
        .headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap();
    let jwt = auth.strip_prefix("Bearer ").unwrap();
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.validate_exp = false;
    let decoded = jsonwebtoken::decode::<serde_json::Value>(
        jwt,
        &jsonwebtoken::DecodingKey::from_rsa_pem(include_bytes!(
            "../../runwell-scaleset/tests/fixtures/app-test-public.pem"
        ))
        .unwrap(),
        &validation,
    )
    .unwrap();
    assert_eq!(decoded.claims["iss"], "5");
    assert!(
        decoded.claims["exp"].as_u64().unwrap() - decoded.claims["iat"].as_u64().unwrap() <= 600
    );
}
#[tokio::test]
async fn paginates_attempt_inventory_and_rejects_partial_or_duplicate_pages() {
    let server = MockServer::start().await;
    Mock::given(path("/repos/a/repo/actions/runs/42/attempts/2/jobs"))
        .and(query_param("page", "1"))
        .and(query_param("per_page", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"total_count":2,"jobs":[{"id":1,"status":"completed","conclusion":"failure"}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(path("/repos/a/repo/actions/runs/42/attempts/2/jobs"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"total_count":2,"jobs":[{"id":2,"status":"completed","conclusion":"success"}]}),
        ))
        .mount(&server)
        .await;
    assert_eq!(
        client(&server)
            .attempt_jobs("a/repo", 42, 2)
            .await
            .unwrap()
            .len(),
        2
    );
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"total_count":2,"jobs":[{"id":1,"status":"completed","conclusion":"failure"}]}),
        ))
        .mount(&server)
        .await;
    assert!(client(&server).attempt_jobs("a/repo", 42, 2).await.is_err());
}
#[tokio::test]
async fn credentials_do_not_follow_redirects_and_existing_file_auth_loads() {
    let server = MockServer::start().await;
    let sink = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(307).insert_header("Location", sink.uri()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&sink)
        .await;
    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), "secret\n").unwrap();
    let config = runwell_config::AuthConfig::Pat {
        token_file: temp.path().into(),
    };
    let api = RestClient::from_config(server.uri().parse().unwrap(), &config)
        .await
        .unwrap();
    assert!(api.rerun_failed_jobs("a/repo", 1).await.is_err());
    assert!(api.get("https://example.com/stolen").await.is_err());
    assert!(api.rerun_failed_jobs("../repo", 1).await.is_err());
}
