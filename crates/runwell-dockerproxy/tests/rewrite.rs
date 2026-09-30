use runwell_dockerproxy::*;
use serde_json::{Value, json};
fn policy() -> Rewriter {
    Rewriter::new(spec(), DockerProxyConfig::default(), CgroupDriver::Systemd).unwrap()
}
fn spec() -> ProxySpec {
    ProxySpec {
        job_id: 42,
        node: "node-a".into(),
        cgroup_parent: "ci-rw-j42.slice".into(),
        memory_max: 1024,
        uid: 501,
        gid: 20,
    }
}
#[test]
fn paths_match_only_exact_versioned_or_unversioned_post_operations() {
    for prefix in ["", "/v1.24", "/v1.99"] {
        for (path, kind) in [
            ("/containers/create", Rewrite::Container),
            ("/networks/create", Rewrite::Labels),
            ("/volumes/create", Rewrite::Labels),
            ("/build", Rewrite::Build),
            ("/containers/abc/update", Rewrite::Update),
        ] {
            assert_eq!(route("POST", &format!("{prefix}{path}")), Some(kind));
            assert_eq!(route("GET", &format!("{prefix}{path}")), None);
        }
    }
    for path in [
        "/v2.1/build",
        "/v1./build",
        "/v1.x/build",
        "/build/",
        "/containers//update",
        "/containers/a/b/update",
        "/session",
        "/grpc",
        "/containers/a/archive",
    ] {
        assert_eq!(route("POST", path), None);
    }
}
#[test]
fn golden_json_before_and_after() {
    for (kind, before, after) in [
        (
            Rewrite::Container,
            include_str!("fixtures/container.before.json"),
            include_str!("fixtures/container.after.json"),
        ),
        (
            Rewrite::Labels,
            include_str!("fixtures/network.before.json"),
            include_str!("fixtures/network.after.json"),
        ),
        (
            Rewrite::Labels,
            include_str!("fixtures/volume.before.json"),
            include_str!("fixtures/volume.after.json"),
        ),
    ] {
        let actual: Value =
            serde_json::from_slice(&policy().json(kind, before.as_bytes()).unwrap()).unwrap();
        assert_eq!(actual, serde_json::from_str::<Value>(after).unwrap());
    }
}
#[test]
fn memory_caps_unlimited_and_excessive_values_preserving_other_limits() {
    for (input, expected) in [(0, 1024), (2048, 1024), (512, 512)] {
        let bytes = serde_json::to_vec(
            &json!({"HostConfig":{"Memory":input,"MemorySwap":8192,"NanoCpus":123,"PidsLimit":9}}),
        )
        .unwrap();
        let result: Value =
            serde_json::from_slice(&policy().json(Rewrite::Container, &bytes).unwrap()).unwrap();
        assert_eq!(
            result["HostConfig"],
            json!({"Memory":expected,"MemorySwap":8192,"NanoCpus":123,"PidsLimit":9,"CgroupParent":"ci-rw-j42.slice"})
        );
    }
    let settings = DockerProxyConfig {
        cap_memory: false,
        ..Default::default()
    };
    let rewriter = Rewriter::new(spec(), settings, CgroupDriver::Systemd).unwrap();
    let value: Value =
        serde_json::from_slice(&rewriter.json(Rewrite::Container, b"{}").unwrap()).unwrap();
    assert!(value["HostConfig"].get("Memory").is_none());
}
#[test]
fn socket_remapping_preserves_targets_options_and_unrelated_mounts() {
    let settings = DockerProxyConfig {
        upstream_socket: "/custom/daemon.sock".into(),
        ..Default::default()
    };
    let rewriter = Rewriter::new(spec(), settings, CgroupDriver::Systemd).unwrap();
    let input = json!({"HostConfig": {
        "Binds": ["/run/docker.sock:/var/run/docker.sock:ro,z", "/custom/daemon.sock:/api.sock", "/work:/work:rw"],
        "Mounts": [{"Type":"bind","Source":"/var/run/docker.sock","Target":"/nested.sock","ReadOnly":true}, {"Type":"volume","Source":"/run/docker.sock","Target":"/data"}]
    }});
    let result: Value = serde_json::from_slice(
        &rewriter
            .json(Rewrite::Container, input.to_string().as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        result["HostConfig"]["Binds"],
        json!([
            "/run/runwell/jobs/42/docker.sock:/var/run/docker.sock:ro,z",
            "/run/runwell/jobs/42/docker.sock:/api.sock",
            "/work:/work:rw"
        ])
    );
    assert_eq!(
        result["HostConfig"]["Mounts"],
        json!([{"Type":"bind","Source":"/run/runwell/jobs/42/docker.sock","Target":"/nested.sock","ReadOnly":true}, {"Type":"volume","Source":"/run/docker.sock","Target":"/data"}])
    );
}
#[test]
fn build_query_golden_preserves_other_encoded_parameters_and_merges_labels() {
    let before = "/v1.47/build?t=a%2fb&t=two+tags&cgroupparent=escape&labels=%7B%22user%22%3A%22yes%22%2C%22io.runwell.job%22%3A%22escape%22%7D&cgroupparent=again&version=2";
    let after = "/v1.47/build?t=a%2fb&t=two+tags&version=2&cgroupparent=ci-rw-j42.slice&labels=%7B%22io.runwell.job%22%3A%2242%22%2C%22io.runwell.node%22%3A%22node-a%22%2C%22user%22%3A%22yes%22%7D";
    assert_eq!(policy().build_query(before).unwrap(), after);
    assert!(policy().build_query("/build?labels=invalid").is_err());
}
#[test]
fn updates_refuse_parent_and_preserve_other_bodies_exactly() {
    for body in [
        r#"{"CgroupParent":"escape"}"#,
        r#"{"cgroupParent":"escape"}"#,
        r#"{"HostConfig":{"CgroupParent":null}}"#,
    ] {
        assert!(matches!(
            policy().json(Rewrite::Update, body.as_bytes()),
            Err(Error::CgroupUpdate)
        ));
    }
    let original = b"{  \"Memory\": 4096 }\n";
    assert_eq!(policy().json(Rewrite::Update, original).unwrap(), original);
}
#[test]
fn case_aliases_cannot_override_forced_fields() {
    let input = br#"{"HostConfig":{"cgroupParent":"escape"},"hostconfig":{"Memory":9999},"labels":{"io.runwell.job":"escape"}}"#;
    let result: Value =
        serde_json::from_slice(&policy().json(Rewrite::Container, input).unwrap()).unwrap();
    assert!(result.get("hostconfig").is_none());
    assert_eq!(result["HostConfig"]["CgroupParent"], "ci-rw-j42.slice");
    assert_eq!(result["Labels"]["io.runwell.job"], "42");
}
#[test]
fn body_size_cap_and_invalid_json_are_clear_errors() {
    let rewriter = Rewriter::new(
        spec(),
        DockerProxyConfig {
            max_json_bytes: 2,
            ..Default::default()
        },
        CgroupDriver::Systemd,
    )
    .unwrap();
    assert!(rewriter.json(Rewrite::Labels, b"{}").is_ok());
    assert!(matches!(
        rewriter.json(Rewrite::Labels, b"{} "),
        Err(Error::BodyTooLarge(2))
    ));
    for input in ["[]", "null", "invalid", "{\"Labels\":[]}"] {
        assert!(policy().json(Rewrite::Labels, input.as_bytes()).is_err());
    }
}
#[test]
fn driver_formats_and_environment() {
    assert_eq!(
        CgroupDriver::Cgroupfs.parent("ci-rw-j42.slice").unwrap(),
        "/ci.slice/ci-rw.slice/ci-rw-j42.slice"
    );
    assert!(CgroupDriver::parse("unknown").is_err());
    assert!(CgroupDriver::Systemd.parent("../../escape").is_err());
    assert_eq!(
        spec().environment(&DockerProxyConfig::default()),
        vec![
            "DOCKER_HOST=unix:///run/runwell/jobs/42/docker.sock",
            "TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE=/run/runwell/jobs/42/docker.sock"
        ]
    );
}
#[test]
fn optional_host_access_policy_defaults_to_compatible() {
    for host in [
        json!({"Privileged":true}),
        json!({"PidMode":"host"}),
        json!({"NetworkMode":"host"}),
    ] {
        let input = json!({"HostConfig":host}).to_string();
        assert!(policy().json(Rewrite::Container, input.as_bytes()).is_ok());
        let rewriter = Rewriter::new(
            spec(),
            DockerProxyConfig {
                deny_host_access: true,
                ..Default::default()
            },
            CgroupDriver::Systemd,
        )
        .unwrap();
        assert!(matches!(
            rewriter.json(Rewrite::Container, input.as_bytes()),
            Err(Error::HostAccess)
        ));
    }
}
