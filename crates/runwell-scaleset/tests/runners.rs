mod common;
use common::*;
use runwell_scaleset::{JitSettings, RemoveRunnerResult};
use serde_json::json;
use wiremock::{Mock, ResponseTemplate, matchers::*};

fn settings() -> JitSettings {
    JitSettings {
        name: "test-runner".into(),
        work_folder: "_work".into(),
    }
}

#[tokio::test]
async fn duplicate_jit_post_recovers_our_runner_and_regenerates_once() {
    let (server, client, _) = harness().await;
    Mock::given(method("POST"))
        .and(path(format!("{SETS}/42/generatejitconfig")))
        .and(body_json(
            json!({"name":"test-runner","workFolder":"_work"}),
        ))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(409).set_body_json(fixture("exists")),
            ResponseTemplate::new(200).set_body_json(fixture("jit")),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(AGENTS))
        .and(query_param("agentName", "test-runner"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"count":1,"value":[fixture("jit")["runner"]]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{AGENTS}/1234")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let config = client.generate_jit_config(42, &settings()).await.unwrap();
    assert_eq!(config.runner.id, 1234);
    assert!(!format!("{config:?}").contains("synthetic-jit-secret"));
    let paths: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().contains("_apis"))
        .map(|r| (r.method.to_string(), r.url.path().to_string()))
        .collect();
    assert_eq!(
        paths.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(),
        vec!["POST", "GET", "DELETE", "POST"]
    );
    versions(&server).await;
}

#[tokio::test]
async fn jit_collision_never_deletes_foreign_or_busy_runner() {
    for foreign in [true, false] {
        let (server, client, _) = harness().await;
        Mock::given(method("POST"))
            .and(path(format!("{SETS}/42/generatejitconfig")))
            .respond_with(ResponseTemplate::new(409).set_body_json(fixture("exists")))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(AGENTS))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"count":1,"value":[{
                "id":1234,"name":"test-runner","runnerScaleSetId":if foreign {43}else{42}}]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("{AGENTS}/1234")))
            .respond_with(ResponseTemplate::new(409).set_body_json(fixture("busy")))
            .expect(if foreign { 0 } else { 1 })
            .mount(&server)
            .await;
        assert!(
            client
                .generate_jit_config(42, &settings())
                .await
                .unwrap_err()
                .is_type("AgentExistsException")
        );
    }
}

#[tokio::test]
async fn jit_recovery_is_bounded_even_when_collision_repeats() {
    let (server, client, _) = harness().await;
    Mock::given(method("POST"))
        .and(path(format!("{SETS}/42/generatejitconfig")))
        .respond_with(ResponseTemplate::new(409).set_body_json(fixture("exists")))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(AGENTS))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"count":0,"value":[]})))
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        client
            .generate_jit_config(42, &settings())
            .await
            .unwrap_err()
            .is_type("AgentExistsException")
    );
}

#[tokio::test]
async fn scale_down_race_job_still_running_is_keep_running() {
    let (server, client, _) = harness().await;
    Mock::given(method("DELETE"))
        .and(path(format!("{AGENTS}/1234")))
        .respond_with(ResponseTemplate::new(409).set_body_json(fixture("busy")))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client.remove_runner(1234).await.unwrap(),
        RemoveRunnerResult::KeepRunning
    );
}

#[tokio::test]
async fn removal_204_and_404_are_safe_including_exit_without_job() {
    let (server, client, _) = harness().await;
    Mock::given(method("DELETE"))
        .and(path(format!("{AGENTS}/1234")))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(204),
            ResponseTemplate::new(404),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    assert_eq!(
        client.remove_runner(1234).await.unwrap(),
        RemoveRunnerResult::SafeToKill
    );
    assert_eq!(
        client.remove_runner(1234).await.unwrap(),
        RemoveRunnerResult::SafeToKill
    );
}

#[tokio::test]
async fn unrelated_removal_conflict_remains_typed_error() {
    let (server, client, _) = harness().await;
    Mock::given(method("DELETE"))
        .and(path(format!("{AGENTS}/1234")))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(json!({"typeName":"DifferentException"})),
        )
        .mount(&server)
        .await;
    let error = client.remove_runner(1234).await.unwrap_err();
    assert!(error.is_type("DifferentException"));
    assert_eq!(error.status().unwrap().as_u16(), 409);
}
