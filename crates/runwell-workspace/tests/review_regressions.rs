mod support;
use runwell_workspace::{Cache, Error, Promotion};
use std::{collections::HashSet, fs};
use support::*;

#[test]
fn corrupt_lease_does_not_undo_publish_and_reconcile_finishes_other_jobs() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = settings(temp.path());
    config.keep_generations = 1;
    let mut cache = Cache::copy(config, &temp.path().join("run"), owner()).unwrap();
    let old = cache.current(&key()).unwrap();
    let broken = cache.prepare(1, Some(key())).unwrap();
    let healthy = cache.prepare(2, Some(key())).unwrap();
    put(&healthy.join("warm"), b"ok");
    fs::write(temp.path().join("run/state/1.json"), b"broken").unwrap();
    assert!(matches!(
        cache.promote(2, &success(), 1).unwrap(),
        Promotion::Published(_)
    ));
    assert_eq!(
        fs::read(cache.current(&key()).unwrap().join("warm")).unwrap(),
        b"ok"
    );
    let error = cache.reconcile(&HashSet::new()).unwrap_err();
    assert!(matches!(error, Error::Reconcile(ids) if ids == [1]));
    assert!(broken.exists());
    assert!(!healthy.exists());
    assert!(!old.exists(), "a corrupt copy-job lease must not block GC");
}

#[cfg(unix)]
#[test]
fn damaged_key_does_not_fail_publish_or_block_other_keys_gc() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = settings(temp.path());
    config.keep_generations = 1;
    let mut cache = Cache::copy(config, &temp.path().join("run"), owner()).unwrap();
    let old = cache.current(&key()).unwrap();
    cache.prepare(1, Some(key())).unwrap();
    let broken = temp.path().join("cache/broken");
    fs::create_dir(&broken).unwrap();
    std::os::unix::fs::symlink("../outside", broken.join("current")).unwrap();
    assert!(matches!(
        cache.promote(1, &success(), 1).unwrap(),
        Promotion::Published(_)
    ));
    cache.teardown(1).unwrap();
    cache.gc().unwrap();
    assert!(!old.exists());
    assert!(fs::symlink_metadata(broken.join("current")).is_ok());
}

#[test]
fn entry_and_depth_caps_reject_empty_directory_forests_without_switching() {
    for (entries, depth, path) in [(2, 64, "a/b/c"), (100, 2, "a/b/c")] {
        let temp = tempfile::tempdir().unwrap();
        let mut config = settings(temp.path());
        config.max_generation_entries = entries;
        config.max_generation_depth = depth;
        let mut cache = Cache::copy(config, &temp.path().join("run"), owner()).unwrap();
        let current = cache.current(&key()).unwrap();
        let home = cache.prepare(1, Some(key())).unwrap();
        fs::create_dir_all(home.join(path)).unwrap();
        assert_eq!(
            cache.promote(1, &success(), 1).unwrap(),
            Promotion::TooLarge
        );
        assert_eq!(cache.current(&key()).unwrap(), current);
        assert!(!current.parent().unwrap().join(".stage").exists());
    }
}

#[test]
fn entry_cap_includes_unchanged_current_and_accepts_the_exact_limit() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = settings(temp.path());
    config.max_generation_entries = 2;
    let mut cache = Cache::copy(config, &temp.path().join("run"), owner()).unwrap();
    let first = cache.prepare(1, Some(key())).unwrap();
    let second = cache.prepare(2, Some(key())).unwrap();
    put(&first.join("one"), b"1");
    put(&first.join("two"), b"2");
    assert!(matches!(
        cache.promote(1, &success(), 1).unwrap(),
        Promotion::Published(_)
    ));
    let current = cache.current(&key()).unwrap();
    put(&second.join("three"), b"3");
    assert_eq!(
        cache.promote(2, &success(), 2).unwrap(),
        Promotion::TooLarge
    );
    assert_eq!(cache.current(&key()).unwrap(), current);
}

#[test]
fn pull_requests_publish_to_separate_key_and_interval() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = settings(temp.path());
    config.allow_pull_requests = true;
    config.promotion_interval_seconds = 100;
    let mut cache = Cache::copy(config, &temp.path().join("run"), owner()).unwrap();
    let first = cache.prepare(1, Some(key())).unwrap();
    put(&first.join("warm"), b"trusted");
    cache.promote(1, &success(), 100).unwrap();
    let default = cache.current(&key()).unwrap();
    let pr = cache.prepare(2, Some(key())).unwrap();
    put(&pr.join("warm"), b"PR");
    let mut completion = success();
    completion.event = "pull_request".into();
    assert!(matches!(
        cache.promote(2, &completion, 101).unwrap(),
        Promotion::Published(_)
    ));
    let pr_current = cache.current(&key().pull_requests()).unwrap();
    assert_ne!(default.parent(), pr_current.parent());
    assert!(
        pr_current
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("-pr")
    );
    assert_eq!(fs::read(pr_current.join("warm")).unwrap(), b"PR");
    assert_eq!(cache.current(&key()).unwrap(), default);
    let next = cache.prepare(3, Some(key())).unwrap();
    assert_eq!(fs::read(next.join("warm")).unwrap(), b"trusted");
}
