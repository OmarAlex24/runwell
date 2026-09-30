mod common;
use common::*;
use runwell_scaleset::{ActionsClient, Config, Credentials, Secret};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};

#[tokio::test]
async fn admin_401_refreshes_and_retries_once_with_new_token() {
    let (server, client, _) = harness().await;
    Mock::given(method("POST"))
        .and(path("/actions/runner-registration"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(200)
                .set_body_json(json!({"url":server.uri(),"token":jwt(NOW+3600)})),
            ResponseTemplate::new(200)
                .set_body_json(json!({"url":server.uri(),"token":jwt(NOW+7200)})),
        ]))
        .with_priority(1)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{SETS}/42")))
        .and(header(
            "Authorization",
            format!("Bearer {}", jwt(NOW + 3600)),
        ))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{SETS}/42")))
        .and(header(
            "Authorization",
            format!("Bearer {}", jwt(NOW + 7200)),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("set")))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(client.get_scale_set(42).await.unwrap().id, 42);
    assert_eq!(
        count(
            &server,
            "POST",
            "/repos/example-org/example-repo/actions/runners/registration-token"
        )
        .await,
        2
    );
    versions(&server).await;
}

#[tokio::test]
async fn repeated_admin_401_stops_after_one_refresh() {
    let (server, client, _) = harness().await;
    Mock::given(method("GET"))
        .and(path(format!("{SETS}/42")))
        .respond_with(ResponseTemplate::new(401))
        .expect(2)
        .mount(&server)
        .await;
    assert_eq!(
        client
            .get_scale_set(42)
            .await
            .unwrap_err()
            .status()
            .unwrap()
            .as_u16(),
        401
    );
    assert_eq!(
        count(&server, "POST", "/actions/runner-registration").await,
        2
    );
}

#[tokio::test]
async fn admin_expiry_refreshes_at_sixty_seconds_and_is_single_flight() {
    let (server, client, clock) = harness().await;
    Mock::given(method("GET"))
        .and(path(format!("{SETS}/42")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("set")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/actions/runner-registration"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(200)
                .set_body_json(json!({"url":server.uri(),"token":jwt(NOW+3600)})),
            ResponseTemplate::new(200)
                .set_body_json(json!({"url":server.uri(),"token":jwt(NOW+7200)})),
        ]))
        .with_priority(1)
        .expect(2)
        .mount(&server)
        .await;
    client.get_scale_set(42).await.unwrap();
    clock.advance(Duration::from_secs(3539));
    client.get_scale_set(42).await.unwrap();
    assert_eq!(
        count(&server, "POST", "/actions/runner-registration").await,
        1
    );
    clock.advance(Duration::from_secs(1));
    let (a, b) = tokio::join!(client.get_scale_set(42), client.get_scale_set(42));
    a.unwrap();
    b.unwrap();
}

#[tokio::test]
async fn concurrent_admin_401s_share_one_refresh() {
    let (server, client, _) = harness().await;
    Mock::given(method("POST"))
        .and(path("/actions/runner-registration"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(200)
                .set_body_json(json!({"url":server.uri(),"token":jwt(NOW+3600)})),
            ResponseTemplate::new(200)
                .set_body_json(json!({"url":server.uri(),"token":jwt(NOW+7200)}))
                .set_delay(Duration::from_millis(30)),
        ]))
        .with_priority(1)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{SETS}/42")))
        .and(header(
            "Authorization",
            format!("Bearer {}", jwt(NOW + 3600)),
        ))
        .respond_with(ResponseTemplate::new(401))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{SETS}/42")))
        .and(header(
            "Authorization",
            format!("Bearer {}", jwt(NOW + 7200)),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("set")))
        .expect(2)
        .mount(&server)
        .await;
    let (a, b) = tokio::join!(client.get_scale_set(42), client.get_scale_set(42));
    a.unwrap();
    b.unwrap();
}

#[tokio::test]
async fn github_app_signs_rs256_and_exchanges_installation_then_registration() {
    let server = MockServer::start().await;
    let credentials = Credentials::App {
        client_id: "test-client-id".into(),
        installation_id: 7,
        private_key: Secret::new(include_str!("fixtures/app-test-key.pem")),
    };
    let mut cfg = Config::new("https://github.com/example-org", credentials).unwrap();
    cfg.github_api_url = server.uri().parse().unwrap();
    cfg.clock = Arc::new(FakeClock::default());
    Mock::given(method("POST"))
        .and(path("/app/installations/7/access_tokens"))
        .and(header("Content-Type", "application/vnd.github+json"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"token":"installation-token"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/orgs/example-org/actions/runners/registration-token"))
        .and(header("Authorization", "Bearer installation-token"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"token":"registration-token"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/actions/runner-registration"))
        .and(header("Authorization", "RemoteAuth registration-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"url":server.uri(),"token":jwt(NOW+3600)})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{SETS}/42")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("set")))
        .mount(&server)
        .await;
    ActionsClient::new(cfg)
        .unwrap()
        .get_scale_set(42)
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let auth = requests[0]
        .headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap();
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.validate_exp = false;
    let claims = jsonwebtoken::decode::<serde_json::Value>(
        auth,
        &jsonwebtoken::DecodingKey::from_rsa_pem(include_bytes!("fixtures/app-test-public.pem"))
            .unwrap(),
        &validation,
    )
    .unwrap()
    .claims;
    assert_eq!(
        claims,
        json!({"iss":"test-client-id","iat":NOW-60,"exp":NOW+480})
    );
    versions(&server).await;
}

#[tokio::test]
async fn registration_propagation_retries_only_explicit_401_and_403() {
    let (server, client, clock) = harness().await;
    Mock::given(method("POST"))
        .and(path("/actions/runner-registration"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(401),
            ResponseTemplate::new(403),
            ResponseTemplate::new(200)
                .set_body_json(json!({"url":server.uri(),"token":jwt(NOW+3600)})),
        ]))
        .with_priority(1)
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{SETS}/42")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("set")))
        .mount(&server)
        .await;
    client.get_scale_set(42).await.unwrap();
    assert_eq!(clock.sleeps.lock().unwrap().len(), 2);
}

#[test]
fn scope_parsing_and_enterprise_app_rejection() {
    assert!(
        Config::new(
            "https://github.com/a/b/c",
            Credentials::Pat(Secret::new("x"))
        )
        .is_err()
    );
    assert!(
        Config::new(
            "https://github.com/enterprises/company",
            Credentials::App {
                client_id: "id".into(),
                installation_id: 1,
                private_key: Secret::new("key")
            }
        )
        .is_err()
    );
    assert_eq!(
        Config::new(
            "https://ghe.example/org",
            Credentials::Pat(Secret::new("x"))
        )
        .unwrap()
        .github_api_url
        .as_str(),
        "https://ghe.example/api/v3/"
    );
}
