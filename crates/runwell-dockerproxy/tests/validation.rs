#![cfg(unix)]
mod support;
use hyper::{Request, StatusCode};
use runwell_dockerproxy::*;
use serde_json::{Value, json};
use support::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn decoded_routes_rewrite_and_ambiguous_paths_return_400() {
    let mut fixture = Fixture::new(echo).await;
    fixture.start().await;
    for path in [
        "/%63ontainers/create",
        "/v1.45.0/containers/create",
        "/v01.45/containers/create",
        "/%761.45/containers/%63reate",
    ] {
        let response = fixture.send("POST", path, full("{}")).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let value: Value = serde_json::from_slice(&bytes(response).await).unwrap();
        assert_eq!(value["HostConfig"]["CgroupParent"], "ci-rw-j42.slice");
        assert_eq!(value["Labels"]["io.runwell.job"], "42");
    }
    for path in [
        "//containers/create",
        "/v1.45//containers/create",
        "/./containers/create",
        "/a/../containers/create",
        "/%2e/containers/create",
        "/a/%2E%2e/containers/create",
        "/containers%2fcreate",
        "/containers%2Fcreate",
        "/%ZZ/containers/create",
    ] {
        let response = fixture.send("POST", path, full("{}")).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        assert!(String::from_utf8_lossy(&bytes(response).await).contains("path segments"));
    }
    // Moby routes are case sensitive, so decoding does not lowercase them.
    for path in ["/Containers/create", "/%43ontainers/create"] {
        let response = fixture.send("POST", path, full("{ }\n")).await;
        assert_eq!(bytes(response).await, "{ }\n");
    }
    fixture.stop().await;
}

#[tokio::test]
async fn unicode_folded_and_duplicate_keys_return_400_before_upstream() {
    let mut fixture = Fixture::new(echo).await;
    fixture.start().await;
    for body in [
        r#"{"HoſtConfig":{"CgroupParent":"escape"}}"#,
        r#"{"Labelſ":{}}"#,
        r#"{"HostConfig":{"NetworKMode":"host"}}"#,
        r#"{"HostConfig":{"Bindſ":[]}}"#,
        r#"{"HostConfig":{"Mountſ":[]}}"#,
        r#"{"HostConfig":{"Mounts":[{"Type":"bind","ſource":"/run/docker.sock"}]}}"#,
        r#"{"Labels":{"K":"value"}}"#,
        r#"{"HostConfig":{},"hostconfig":{}}"#,
        r#"{"Labels":{},"labels":{}}"#,
        r#"{"HostConfig":{"CgroupParent":"x","cgroupparent":"y"}}"#,
        r#"{"HostConfig":{"Memory":1,"Memory":2}}"#,
        r#"{"HostConfig":{"NetworkMode":"host","networkmode":"bridge"}}"#,
        r#"{"HostConfig":{"Mounts":[{"Source":"/a","source":"/b"}]}}"#,
        r#"{"Labels":{"io.runwell.job":"1","IO.RUNWELL.JOB":"2"}}"#,
        r#"{"Ho\u017ftConfig":{}}"#,
    ] {
        let response = fixture.send("POST", "/containers/create", full(body)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        assert!(String::from_utf8_lossy(&bytes(response).await).contains("object keys"));
    }
    for path in [
        "/networks/create",
        "/volumes/create",
        "/containers/id/update",
    ] {
        for body in [r#"{"K":1}"#, r#"{"Labels":{},"LABELS":{}}"#] {
            assert_eq!(
                fixture.send("POST", path, full(body)).await.status(),
                StatusCode::BAD_REQUEST
            );
        }
    }
    for labels in [
        r#"{"ſ":"x"}"#,
        r#"{"io.runwell.job":"1","io.runwell.job":"2"}"#,
    ] {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("labels", labels)
            .finish();
        assert_eq!(
            fixture
                .send("POST", &format!("/build?{query}"), full("tar"))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    fixture.stop().await;
}

#[tokio::test]
async fn upgrades_on_mutations_and_unrelated_routes_are_rejected() {
    let mut fixture = Fixture::new(|_| async { panic!("rejected upgrade reached upstream") }).await;
    fixture.start().await;
    for path in [
        "/containers/create",
        "/%63ontainers/create",
        "/v01.45/containers/create",
        "/build",
        "/networks/create",
        "/volumes/create",
        "/containers/id/update",
        "/info",
        "/containers/id/archive",
        "/exec/id/start/extra",
        "/session/extra",
    ] {
        for protocol in ["tcp", "h2c"] {
            let response = send(
                &fixture.socket(),
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("Host", "docker")
                    .header("Connection", "Upgrade")
                    .header("Upgrade", protocol)
                    .body(full("{}"))
                    .unwrap(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        }
    }
    let response = send(
        &fixture.socket(),
        Request::builder()
            .method("POST")
            .uri("/containers/create")
            .header("Host", "docker")
            .header("Connection", "keep-alive, UpGrAdE")
            .body(full("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    fixture.stop().await;
}

#[tokio::test]
async fn stalled_buffered_json_has_a_read_deadline() {
    let mut fixture = Fixture::new(|_| async { panic!("incomplete JSON reached upstream") }).await;
    fixture.settings.body_read_seconds = 1;
    fixture.start().await;
    for framing in [
        "Content-Length: 10\r\n\r\n{",
        "Transfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n",
    ] {
        let mut stream = tokio::net::UnixStream::connect(fixture.socket())
            .await
            .unwrap();
        stream
            .write_all(
                format!("POST /containers/create HTTP/1.1\r\nHost: docker\r\n{framing}").as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            stream.read_to_string(&mut response),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(response.starts_with("HTTP/1.1 408"), "{response}");
        assert!(response.contains("JSON body read timed out"));
    }
    fixture.stop().await;
}

#[tokio::test]
async fn lexically_equivalent_socket_sources_are_remapped() {
    let fixture = Fixture::new(echo).await;
    let policy = Rewriter::new(
        fixture.spec.clone(),
        fixture.settings.clone(),
        CgroupDriver::Systemd,
    )
    .unwrap();
    for source in [
        "/run//docker.sock/".to_string(),
        "/var/./run/docker.sock".into(),
        "/var/run/../run/docker.sock/".into(),
        "/../../run/docker.sock".into(),
        format!(
            "{}/missing/../up.sock/",
            fixture.dir.path().canonicalize().unwrap().display()
        ),
    ] {
        let input = json!({"HostConfig":{"Binds":[format!("{source}:/docker.sock:ro")],
            "Mounts":[{"Type":"bind","Source":source,"Target":"/api.sock"}]}});
        let result: Value = serde_json::from_slice(
            &policy
                .json(Rewrite::Container, input.to_string().as_bytes())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            result["HostConfig"]["Binds"][0],
            format!("{}:/docker.sock:ro", fixture.socket().display())
        );
        assert_eq!(
            result["HostConfig"]["Mounts"][0]["Source"],
            fixture.socket().to_str().unwrap()
        );
    }
}

#[tokio::test]
async fn unlimited_slice_memory_keeps_container_limits() {
    let mut fixture = Fixture::new(echo).await;
    fixture.spec.memory_max = ProxySpec::parse_memory_max("max\n").unwrap();
    assert_eq!(fixture.spec.memory_max, None);
    let policy = Rewriter::new(
        fixture.spec.clone(),
        fixture.settings.clone(),
        CgroupDriver::Systemd,
    )
    .unwrap();
    for host in [json!({}), json!({"Memory":0}), json!({"Memory":4096})] {
        let input = json!({"HostConfig":host});
        let result: Value = serde_json::from_slice(
            &policy
                .json(Rewrite::Container, input.to_string().as_bytes())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["HostConfig"].get("Memory"), host.get("Memory"));
    }
    assert_eq!(ProxySpec::parse_memory_max("1024\n").unwrap(), Some(1024));
    assert!(ProxySpec::parse_memory_max("invalid").is_err());
}
