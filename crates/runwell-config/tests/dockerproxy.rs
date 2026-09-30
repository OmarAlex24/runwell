use runwell_config::Config;
const EXAMPLE: &str = include_str!("../../../examples/runwell.toml");
#[test]
fn docker_proxy_defaults_and_explicit_settings() {
    let config = Config::from_toml(EXAMPLE).unwrap();
    let proxy = config.standalone.unwrap().docker_proxy;
    assert_eq!(
        proxy.upstream_socket.to_str().unwrap(),
        "/var/run/docker.sock"
    );
    assert_eq!(proxy.max_json_bytes, 2 * 1024 * 1024);
    assert!(proxy.cap_memory);
    assert!(!proxy.deny_host_access);
    let source = format!(
        "{EXAMPLE}\n[standalone.docker_proxy]\nrun_dir = '/run/custom'\nmax_json_bytes = 4096\ncap_memory = false\ndeny_host_access = true\n"
    );
    let proxy = Config::from_toml(&source)
        .unwrap()
        .standalone
        .unwrap()
        .docker_proxy;
    assert_eq!(proxy.max_json_bytes, 4096);
    assert!(!proxy.cap_memory);
    assert!(proxy.deny_host_access);
}
#[test]
fn docker_proxy_rejects_invalid_and_unknown_settings() {
    for setting in [
        "max_json_bytes = 0",
        "stop_seconds = 0",
        "stop_seconds = 3601",
        "run_dir = 'relative'",
        "run_dir = '/tmp/../run'",
        "upstream_socket = '/'",
        "misspelled = true",
    ] {
        assert!(
            Config::from_toml(&format!(
                "{EXAMPLE}\n[standalone.docker_proxy]\n{setting}\n"
            ))
            .is_err(),
            "{setting}"
        );
    }
}
