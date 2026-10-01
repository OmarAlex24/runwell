use runwell_config::Config;

#[test]
fn topology_rejects_duplicate_members_missing_tls_and_unbounded_deadlines() {
    let config = Config::from_toml(include_str!("../../../examples/runwell.toml")).unwrap();
    let mut invalid = config.clone();
    invalid.transport = None;
    assert!(invalid.validate().is_err());
    let mut invalid = config.clone();
    let network = invalid.network.as_mut().unwrap();
    network.nodes[1].id = network.nodes[0].id.clone();
    assert!(invalid.validate().is_err());
    let mut invalid = config.clone();
    invalid.network.as_mut().unwrap().lost_seconds = u64::MAX;
    assert!(invalid.validate().is_err());
    let mut invalid = config.clone();
    invalid.network.as_mut().unwrap().preparation_seconds = 0;
    assert!(invalid.validate().is_err());
    let mut invalid = config;
    invalid.network.as_mut().unwrap().controller_id = "../escape".into();
    assert!(invalid.validate().is_err());
}
