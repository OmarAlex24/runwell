use runwell_config::Config;
fn config(extra: &str) -> Result<Config, runwell_config::Error> {
    Config::from_toml(&format!(
        "{}\n{extra}",
        include_str!("../../../examples/runwell.toml")
    ))
}
#[test]
fn cache_defaults_are_bounded_and_repository_scoped() {
    let config = config("").unwrap();
    let workspace = config.standalone.unwrap().workspace;
    assert!(!workspace.per_class);
    assert!(!workspace.allow_pull_requests);
    assert_eq!(workspace.promotion_interval_seconds, 21600);
    assert_eq!(workspace.max_generation_bytes, 20 * 1024 * 1024 * 1024);
    assert_eq!(workspace.keep_generations, 2);
    assert_eq!(workspace.max_generation_entries, 1_000_000);
    assert_eq!(workspace.max_generation_depth, 64);
}
#[test]
fn rejects_invalid_or_overlapping_cache_paths_and_limits() {
    for field in [
        "cache_root = 'relative'",
        "cache_root = '/x/../y'",
        "cache_root = '/bad,option'",
        "cache_root = '/var/lib/runwell/runners/cache'",
        "keep_generations = 0",
        "max_generation_bytes = 0",
        "max_generation_entries = 0",
        "max_generation_depth = 0",
        "max_generation_depth = 257",
        "excludes = ['../secret']",
        "unexpected = true",
    ] {
        assert!(
            config(&format!("[standalone.workspace]\n{field}")).is_err(),
            "{field}"
        );
    }
}
#[test]
fn accepts_additive_rules_and_class_routing() {
    let config = config("[standalone.workspace]\nper_class = true\nallow_pull_requests = true\nexcludes = ['private/*']\n[standalone.workspace.repositories]\nrunwell-small = 'owner/repo'").unwrap();
    let workspace = config.standalone.unwrap().workspace;
    assert_eq!(workspace.repositories["runwell-small"], "owner/repo");
}
