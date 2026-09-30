mod common;
use common::*;
use runwell_scaleset::{Label, RunnerSetting, ScaleSet};
use serde_json::json;
use wiremock::{Mock, ResponseTemplate, matchers::*};

#[tokio::test]
async fn scale_set_crud_uses_200_capital_runner_setting_and_default_labels() {
    let (server, client, _) = harness().await;
    Mock::given(method("POST")).and(path(SETS))
        .and(body_json(json!({"name":"test-set","runnerGroupId":1,
            "labels":[{"name":"test-set","type":"System"}],"RunnerSetting":{"disableUpdate":true},"createdOn":"0001-01-01T00:00:00Z"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("set"))).expect(1).mount(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{SETS}/42")))
        .and(body_json(
            json!({"runnerGroupId":2,"labels":[{"name":"linux-x64","type":"System"}],
            "RunnerSetting":{},"createdOn":"0001-01-01T00:00:00Z"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("set")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{SETS}/42")))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(204),
            ResponseTemplate::new(404),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    let created = client
        .create_scale_set(ScaleSet {
            name: "test-set".into(),
            runner_group_id: 1,
            runner_setting: RunnerSetting {
                disable_update: true,
            },
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(created.id, 42);
    client
        .update_scale_set(
            42,
            ScaleSet {
                runner_group_id: 2,
                labels: vec![Label {
                    kind: String::new(),
                    name: "linux-x64".into(),
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    client.delete_scale_set(42).await.unwrap();
    client.delete_scale_set(42).await.unwrap();
    versions(&server).await;
}

#[tokio::test]
async fn lookup_by_name_handles_none_one_duplicates_and_invalid_counts() {
    for (count, values, valid) in [
        (0, vec![], true),
        (1, vec![fixture("set")], true),
        (2, vec![fixture("set"), fixture("set")], false),
        (1, vec![], false),
    ] {
        let (server, client, _) = harness().await;
        Mock::given(method("GET"))
            .and(path(SETS))
            .and(query_param("runnerGroupId", "1"))
            .and(query_param("name", "set with & spaces"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"count":count,"value":values})),
            )
            .mount(&server)
            .await;
        let found = client.get_scale_set_by_name(1, "set with & spaces").await;
        assert_eq!(found.is_ok(), valid);
        if valid {
            assert_eq!(found.unwrap().is_some(), count == 1);
        }
    }
}

#[tokio::test]
async fn list_groups_runner_lookup_and_bom_on_admin_response() {
    let (server, client, _) = harness().await;
    Mock::given(method("GET"))
        .and(path(SETS))
        .and(query_param("runnerGroupId", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "\u{feff}{}",
            json!({"count":1,"value":[fixture("set")]})
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/_apis/runtime/runnergroups/"))
        .and(query_param("groupName", "Default"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"count":1,"value":[{"id":1,"name":"Default","isDefaultGroup":true}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{AGENTS}/1234")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("jit")["runner"].clone()))
        .mount(&server)
        .await;
    assert_eq!(client.list_scale_sets(1).await.unwrap().len(), 1);
    assert!(
        client
            .get_runner_group_by_name("Default")
            .await
            .unwrap()
            .is_default_group
    );
    assert_eq!(
        client.get_runner(1234).await.unwrap().runner_scale_set_id,
        42
    );
}

#[tokio::test]
async fn create_rejects_201_and_empty_identity() {
    let (server, client, _) = harness().await;
    assert!(client.create_scale_set(ScaleSet::default()).await.is_err());
    assert_eq!(count(&server, "POST", SETS).await, 0);
    Mock::given(method("POST"))
        .and(path(SETS))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client
            .create_scale_set(ScaleSet {
                name: "test-set".into(),
                ..Default::default()
            })
            .await
            .unwrap_err()
            .status()
            .unwrap()
            .as_u16(),
        201
    );
}
