use runwell_runner::*;
#[test]
fn checksums_are_bound_to_exact_asset_and_fail_closed() {
    let hash = "ab".repeat(32);
    let body = format!("| actions-runner-linux-x64-2.337.0.tar.gz | {hash} |");
    assert_eq!(checksum("2.337.0", &body, None).unwrap(), [0xab; 32]);
    assert!(checksum("2.338.0", &body, None).is_err());
    assert!(checksum("2.337.0", "no checksum", None).is_err());
    assert_eq!(checksum("2.337.0", "", Some(&hash)).unwrap(), [0xab; 32]);
    assert!(checksum("../../bad", "", Some(&hash)).is_err());
    assert!(
        checksum(
            "2.337.0",
            &format!(
                "{body}\n{} actions-runner-linux-x64-2.337.0.tar.gz",
                "cd".repeat(32)
            ),
            None
        )
        .is_err()
    );
}
fn release(version: &str, date: &str) -> Release {
    serde_json::from_value(serde_json::json!({"tag_name":version,"published_at":date,"body":""}))
        .unwrap()
}
#[test]
fn release_deadline_is_not_reset_by_frequent_releases() {
    let releases = vec![
        release("v2.338.0", "2026-01-01T00:00:00Z"),
        release("v2.339.0", "2026-01-20T00:00:00Z"),
    ];
    for (date, expected) in [
        ("2026-01-21T00:00:00Z", ReleaseStatus::Current),
        ("2026-01-22T00:00:00Z", ReleaseStatus::Warn),
        ("2026-01-26T00:00:00Z", ReleaseStatus::Refresh),
        ("2026-01-31T00:00:00Z", ReleaseStatus::Expired),
    ] {
        assert_eq!(
            release_status("2.337.0", &releases, date.parse().unwrap()).unwrap(),
            expected
        );
    }
    assert_eq!(
        release_status(
            "2.339.0",
            &releases,
            "2027-01-01T00:00:00Z".parse().unwrap()
        )
        .unwrap(),
        ReleaseStatus::Current
    );
}
#[test]
fn installations_share_only_immutable_files_and_have_separate_homes() {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("template");
    fs::create_dir_all(source.join("bin")).unwrap();
    fs::write(source.join("bin/Runner.Listener"), "executable").unwrap();
    fs::write(source.join(".env"), "template env").unwrap();
    let template = Template {
        version: "2.337.0".into(),
        directory: source.clone(),
        sha256: [0; 32],
    };
    let first = dir.path().join("j1");
    let second = dir.path().join("j2");
    clone_install(&template, &first).unwrap();
    clone_install(&template, &second).unwrap();
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(first.join("bin/Runner.Listener"))
            .unwrap()
            .ino(),
        fs::metadata(second.join("bin/Runner.Listener"))
            .unwrap()
            .ino()
    );
    fs::write(first.join(".env"), "job one").unwrap();
    fs::write(first.join("home/cache"), "private").unwrap();
    assert_eq!(
        fs::read_to_string(second.join(".env")).unwrap(),
        "template env"
    );
    assert!(!second.join("home/cache").exists());
    assert_eq!(
        fs::read_to_string(source.join(".env")).unwrap(),
        "template env"
    );
    assert!(clone_install(&template, &first).is_err());
    assert!(remove_install(dir.path(), dir.path()).is_err());
    remove_install(dir.path(), &first).unwrap();
    remove_install(dir.path(), &first).unwrap();
}
#[test]
fn direct_listener_exit_mapping_distinguishes_outdated_from_workflow_success() {
    assert_eq!(classify_exit(Some(0)), ExitDisposition::Exited);
    assert_eq!(classify_exit(Some(7)), ExitDisposition::Outdated);
    assert_eq!(classify_exit(Some(5)), ExitDisposition::Discard);
    assert_eq!(classify_exit(None), ExitDisposition::Transient);
}
