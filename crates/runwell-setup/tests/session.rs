use runwell_setup::{
    facts::HostFacts,
    model::{ProbedHost, SetupSession},
    session,
};

#[test]
fn session_survives_atomic_replacement_and_validates_targets_on_resume() {
    let dir = std::env::temp_dir().join(format!("runwell-session-{}", uuid::Uuid::new_v4()));
    let path = dir.join("state.json");
    assert!(session::load(&path).unwrap().hosts.is_empty());
    let mut state = SetupSession::default();
    state.hosts.push(ProbedHost {
        target: "user@example.invalid:2222".parse().unwrap(),
        facts: HostFacts::default(),
    });
    state.repos.push("example/project".parse().unwrap());
    session::save(&path, &state).unwrap();
    let resumed = session::load(&path).unwrap();
    assert_eq!(
        resumed.hosts[0].target.to_string(),
        "user@example.invalid:2222"
    );
    assert_eq!(resumed.repos[0].0, "example/project");
    state.repos.push("example/second".parse().unwrap());
    session::save(&path, &state).unwrap();
    assert_eq!(session::load(&path).unwrap().repos.len(), 2);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::write(&path, "{\"hosts\":[{\"target\":\"-o@host\",\"facts\":{}}]}").unwrap();
    assert!(session::load(&path).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}
