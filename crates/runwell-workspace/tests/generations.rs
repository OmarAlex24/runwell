mod support;
use runwell_workspace::{Cache, CacheKey, Mode, Promotion};
use std::{collections::HashSet, fs};
use support::*;

#[test]
fn fallback_isolation_atomic_switch_and_deletion_survive_restart() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = cache(temp.path());
    assert_eq!(cache.mode(), Mode::Copy);
    let first = cache.prepare(1, Some(key())).unwrap();
    let second = cache.prepare(2, Some(key())).unwrap();
    put(&first.join("go/pkg/mod/module"), b"warm");
    assert!(!second.join("go/pkg/mod/module").exists());
    assert!(matches!(
        cache.promote(1, &success(), 10).unwrap(),
        Promotion::Published(_)
    ));
    assert!(!second.join("go/pkg/mod/module").exists());
    let third = cache.prepare(3, Some(key())).unwrap();
    let generation = cache.current(&key()).unwrap();
    assert_eq!(fs::read(third.join("go/pkg/mod/module")).unwrap(), b"warm");
    put(&third.join("go/pkg/mod/module"), b"changed");
    assert_eq!(
        fs::read(generation.join("go/pkg/mod/module")).unwrap(),
        b"warm"
    );
    fs::remove_file(third.join("go/pkg/mod/module")).unwrap();
    cache.promote(3, &success(), 20).unwrap();
    assert!(
        !cache
            .current(&key())
            .unwrap()
            .join("go/pkg/mod/module")
            .exists()
    );
    let selected = cache.current(&key()).unwrap();
    assert_eq!(
        fs::read_link(selected.parent().unwrap().join("current")).unwrap(),
        selected.file_name().unwrap()
    );
    drop(cache);
    let mut cache = support::cache(temp.path());
    assert_eq!(cache.current(&key()).unwrap(), selected);
    cache.reconcile(&HashSet::from([2])).unwrap();
    assert!(second.exists());
    assert!(!first.exists());
    assert!(!third.exists());
    cache.teardown(2).unwrap();
    cache.teardown(2).unwrap();
}
#[test]
fn gc_pins_old_generation_until_last_lease_is_released() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = settings(temp.path());
    config.keep_generations = 1;
    let mut cache = Cache::copy(config, &temp.path().join("run"), owner()).unwrap();
    let old = cache.current(&key()).unwrap();
    cache.prepare(1, Some(key())).unwrap();
    cache.prepare(2, Some(key())).unwrap();
    cache.promote(1, &success(), 10).unwrap();
    cache.teardown(1).unwrap();
    cache.gc().unwrap();
    assert!(old.exists());
    drop(cache);
    let mut cache = support::cache(temp.path());
    cache.gc().unwrap();
    assert!(old.exists());
    cache.teardown(2).unwrap();
    let home = cache.prepare(3, Some(key())).unwrap();
    put(&home.join("cache"), b"3");
    cache.promote(3, &success(), 20).unwrap();
    cache.gc().unwrap();
    assert!(!old.exists());
}
#[test]
fn interval_is_per_key_persisted_and_clock_rollback_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = settings(temp.path());
    config.promotion_interval_seconds = 100;
    let mut cache = Cache::copy(config.clone(), &temp.path().join("run"), owner()).unwrap();
    cache.prepare(1, Some(key())).unwrap();
    cache.promote(1, &success(), 1000).unwrap();
    cache.prepare(2, Some(key())).unwrap();
    drop(cache);
    let mut cache = Cache::copy(config, &temp.path().join("run"), owner()).unwrap();
    for now in [900, 1000, 1099] {
        assert_eq!(
            cache.promote(2, &success(), now).unwrap(),
            Promotion::Interval
        );
    }
    let other = CacheKey::new("example/repo", Some("large")).unwrap();
    cache.prepare(3, Some(other)).unwrap();
    assert!(matches!(
        cache.promote(3, &success(), 1001).unwrap(),
        Promotion::Published(_)
    ));
    assert!(matches!(
        cache.promote(2, &success(), 1100).unwrap(),
        Promotion::Published(_)
    ));
}
#[test]
fn failed_cancelled_fork_wrong_ref_and_unbound_jobs_never_promote() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = cache(temp.path());
    cache.prepare(1, Some(key())).unwrap();
    let initial = cache.current(&key()).unwrap();
    let mut failure = success();
    failure.succeeded = false;
    let mut fork = success();
    fork.same_repository = false;
    let mut branch = success();
    branch.branch = "feature".into();
    let mut pr = success();
    pr.event = "pull_request".into();
    let mut repo = success();
    repo.repository = "another/repo".into();
    for completion in [failure, fork, branch, pr, repo] {
        assert_eq!(
            cache.promote(1, &completion, 10).unwrap(),
            Promotion::Untrusted
        );
        assert_eq!(cache.current(&key()).unwrap(), initial);
    }
    cache.prepare(2, None).unwrap();
    assert_eq!(
        cache.promote(2, &success(), 10).unwrap(),
        Promotion::Untrusted
    );
}
#[test]
fn opted_in_same_repository_pr_can_promote() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = settings(temp.path());
    config.allow_pull_requests = true;
    let mut cache = Cache::copy(config, &temp.path().join("run"), owner()).unwrap();
    cache.prepare(1, Some(key())).unwrap();
    let mut completion = success();
    completion.event = "pull_request".into();
    assert!(matches!(
        cache.promote(1, &completion, 10).unwrap(),
        Promotion::Published(_)
    ));
}
#[test]
fn cap_includes_unchanged_generation_and_failed_publish_preserves_current() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = settings(temp.path());
    config.max_generation_bytes = 8;
    let mut cache = Cache::copy(config, &temp.path().join("run"), owner()).unwrap();
    let home = cache.prepare(1, Some(key())).unwrap();
    put(&home.join("one"), b"12345678");
    cache.promote(1, &success(), 10).unwrap();
    let current = cache.current(&key()).unwrap();
    let home = cache.prepare(2, Some(key())).unwrap();
    put(&home.join("two"), b"9");
    assert_eq!(
        cache.promote(2, &success(), 20).unwrap(),
        Promotion::TooLarge
    );
    assert_eq!(cache.current(&key()).unwrap(), current);
    assert!(!current.parent().unwrap().join(".stage").exists());
}
#[test]
fn concurrent_copy_jobs_merge_changes_on_latest_generation() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = cache(temp.path());
    let a = cache.prepare(1, Some(key())).unwrap();
    let b = cache.prepare(2, Some(key())).unwrap();
    put(&a.join("first"), b"a");
    put(&b.join("second"), b"b");
    cache.promote(1, &success(), 10).unwrap();
    cache.promote(2, &success(), 20).unwrap();
    let current = cache.current(&key()).unwrap();
    assert_eq!(fs::read(current.join("first")).unwrap(), b"a");
    assert_eq!(fs::read(current.join("second")).unwrap(), b"b");
}
#[test]
fn manager_lock_prevents_concurrent_publishers() {
    let temp = tempfile::tempdir().unwrap();
    let _cache = cache(temp.path());
    assert!(matches!(
        Cache::copy(settings(temp.path()), &temp.path().join("run"), owner()),
        Err(runwell_workspace::Error::Locked)
    ));
}

#[test]
fn actual_assignment_evidence_is_durable_and_cannot_be_replaced() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = cache(temp.path());
    cache.prepare(1, Some(key())).unwrap();
    let evidence = runwell_workspace::Execution {
        request_id: 42,
        workflow_run_id: 100,
        repository: "example/repo".into(),
    };
    cache.bind(1, evidence.clone()).unwrap();
    drop(cache);
    let mut cache = support::cache(temp.path());
    assert_eq!(cache.execution(1).unwrap(), Some(evidence.clone()));
    let mut other = evidence.clone();
    other.request_id += 1;
    assert!(cache.bind(1, other).is_err());
    assert_eq!(cache.execution(1).unwrap(), Some(evidence));
}

#[cfg(unix)]
#[test]
fn immutable_generations_share_unchanged_inodes_but_replace_modified_files() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    let mut cache = cache(temp.path());
    let first = cache.prepare(1, Some(key())).unwrap();
    put(&first.join("unchanged"), b"warm");
    put(&first.join("changed"), b"old");
    put(&first.join("executable"), b"binary");
    cache.promote(1, &success(), 1).unwrap();
    let before = cache.current(&key()).unwrap();
    let second = cache.prepare(2, Some(key())).unwrap();
    put(&second.join("changed"), b"new");
    fs::set_permissions(second.join("executable"), fs::Permissions::from_mode(0o700)).unwrap();
    cache.promote(2, &success(), 2).unwrap();
    let after = cache.current(&key()).unwrap();
    assert_eq!(
        fs::metadata(before.join("unchanged")).unwrap().ino(),
        fs::metadata(after.join("unchanged")).unwrap().ino()
    );
    assert_ne!(
        fs::metadata(before.join("changed")).unwrap().ino(),
        fs::metadata(after.join("changed")).unwrap().ino()
    );
    assert_eq!(fs::read(before.join("changed")).unwrap(), b"old");
    assert_ne!(
        fs::metadata(after.join("executable"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
}

#[test]
fn unchanged_copy_directory_does_not_undo_a_newer_type_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = cache(temp.path());
    let seed = cache.prepare(1, Some(key())).unwrap();
    put(&seed.join("directory/file"), b"seed");
    cache.promote(1, &success(), 1).unwrap();
    let first = cache.prepare(2, Some(key())).unwrap();
    cache.prepare(3, Some(key())).unwrap();
    fs::remove_dir_all(first.join("directory")).unwrap();
    put(&first.join("directory"), b"replacement");
    cache.promote(2, &success(), 2).unwrap();
    cache.promote(3, &success(), 3).unwrap();
    assert_eq!(
        fs::read(cache.current(&key()).unwrap().join("directory")).unwrap(),
        b"replacement"
    );
}
