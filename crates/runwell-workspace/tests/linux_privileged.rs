#![cfg(target_os = "linux")]
mod support;
use runwell_workspace::{Cache, Mode, Owner, Promotion};
use std::{
    collections::HashSet,
    fs,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    process::Command,
};
use support::*;

fn linux_cache(temp: &Path) -> Cache {
    assert!(rustix::process::geteuid().is_root());
    fs::set_permissions(temp, fs::Permissions::from_mode(0o711)).unwrap();
    let cache = Cache::open(
        settings(temp),
        &temp.join("run"),
        Owner {
            uid: 65534,
            gid: 65534,
        },
    )
    .unwrap();
    assert_eq!(
        cache.mode(),
        Mode::Overlay,
        "privileged CI must support overlayfs"
    );
    cache
}
use std::path::Path;
fn as_job(script: &str, home: &Path) {
    let status = Command::new("/bin/sh")
        .args(["-c", script, "probe"])
        .arg(home)
        .uid(65534)
        .gid(65534)
        .status()
        .unwrap();
    assert!(status.success());
}
#[test]
#[ignore = "requires Linux root with CAP_SYS_ADMIN and overlayfs"]
fn overlays_isolate_promote_delete_redirect_and_have_job_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = linux_cache(temp.path());
    let one = cache.prepare(1, Some(key())).unwrap();
    let two = cache.prepare(2, Some(key())).unwrap();
    assert_eq!(fs::metadata(&one).unwrap().uid(), 65534);
    for name in ["upper", "work"] {
        assert_eq!(
            fs::metadata(one.parent().unwrap().join(name))
                .unwrap()
                .uid(),
            65534
        );
    }
    as_job(
        "mkdir -p \"$1/go/pkg\"; echo warm > \"$1/go/pkg/module\"; echo keep > \"$1/keep\"",
        &one,
    );
    assert!(!two.join("go/pkg/module").exists());
    assert!(matches!(
        cache.promote(1, &success(), 1).unwrap(),
        Promotion::Published(_)
    ));
    let initial = cache.current(&key()).unwrap();
    as_job("test ! -r \"$1\"", &initial.join("keep"));
    let three = cache.prepare(3, Some(key())).unwrap();
    assert_eq!(fs::read(three.join("go/pkg/module")).unwrap(), b"warm\n");
    as_job(
        "echo changed > \"$1/keep\"; rm \"$1/go/pkg/module\"",
        &three,
    );
    assert_eq!(fs::read(initial.join("keep")).unwrap(), b"keep\n");
    let whiteout = three.parent().unwrap().join("upper/go/pkg/module");
    let metadata = fs::symlink_metadata(whiteout).unwrap();
    assert!(metadata.file_type().is_char_device() && metadata.rdev() == 0);
    cache.promote(3, &success(), 2).unwrap();
    let four = cache.prepare(4, Some(key())).unwrap();
    assert!(!four.join("go/pkg/module").exists());
    // Exercise redirect_dir=on on a nonempty lower directory.
    as_job(
        "mkdir -p \"$1/browsers/version\"; echo browser > \"$1/browsers/version/binary\"",
        &four,
    );
    cache.promote(4, &success(), 3).unwrap();
    let five = cache.prepare(5, Some(key())).unwrap();
    as_job("mv \"$1/browsers\" \"$1/moved\"", &five);
    cache.promote(5, &success(), 4).unwrap();
    let six = cache.prepare(6, Some(key())).unwrap();
    assert!(!six.join("browsers").exists());
    assert_eq!(
        fs::read(six.join("moved/version/binary")).unwrap(),
        b"browser\n"
    );
    for id in 1..=6 {
        cache.teardown(id).unwrap();
    }
    assert!(
        !fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .contains(&temp.path().display().to_string())
    );
}
#[test]
#[ignore = "requires Linux root with CAP_SYS_ADMIN and overlayfs"]
fn reconcile_unmounts_stale_mount_but_preserves_live_job_and_cleans_orphans() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = linux_cache(temp.path());
    let stale = cache.prepare(1, Some(key())).unwrap();
    let live = cache.prepare(2, Some(key())).unwrap();
    fs::create_dir_all(temp.path().join("run/jobs/99/upper")).unwrap();
    drop(cache);
    let mut cache = linux_cache(temp.path());
    cache.reconcile(&HashSet::from([2])).unwrap();
    assert!(!stale.exists());
    assert!(live.exists());
    assert!(!temp.path().join("run/jobs/99").exists());
    as_job("echo alive > \"$1/alive\"", &live);
    cache.teardown(2).unwrap();
}

#[test]
#[ignore = "requires Linux root with CAP_SYS_ADMIN and overlayfs"]
fn lazy_unmount_keeps_generation_pinned_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = linux_cache(temp.path());
    let home = cache.prepare(1, Some(key())).unwrap();
    let lower = cache.current(&key()).unwrap();
    let mut child = Command::new("/bin/sleep")
        .arg("30")
        .current_dir(&home)
        .spawn()
        .unwrap();
    let result = cache.teardown(1);
    assert!(matches!(result, Err(runwell_workspace::Error::Detached)));
    drop(cache);
    let mut cache = linux_cache(temp.path());
    cache.reconcile(&HashSet::new()).unwrap();
    assert!(lower.exists());
    assert!(home.parent().unwrap().join("upper").exists());
    child.kill().unwrap();
    child.wait().unwrap();
}
