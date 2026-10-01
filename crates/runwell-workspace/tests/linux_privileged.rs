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
            fs::metadata(temp.path().join("run/upper/1").join(name))
                .unwrap()
                .uid(),
            0
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
    let whiteout = temp.path().join("run/upper/3/upper/home/go/pkg/module");
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
    fs::create_dir_all(temp.path().join("run/upper/99/upper")).unwrap();
    drop(cache);
    let mut cache = linux_cache(temp.path());
    cache.reconcile(&HashSet::from([2])).unwrap();
    assert!(!stale.exists());
    assert!(live.exists());
    assert!(!temp.path().join("run/upper/99").exists());
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
    assert!(temp.path().join("run/upper/1/upper").exists());
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
#[ignore = "requires Linux root, overlayfs, unshare, mount and setpriv"]
fn job_namespace_cannot_open_other_jobs_upper_or_home() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = linux_cache(temp.path());
    let one = cache.prepare(1, Some(key())).unwrap();
    let two = cache.prepare(2, Some(key())).unwrap();
    as_job("echo private > \"$1/private\"", &one);
    let upper = temp.path().join("run/upper/1/upper/home/private");
    as_job("test ! -r \"$1\" && test ! -w \"$1\"", &upper);
    let mut peer = Command::new("/bin/sleep")
        .arg("30")
        .uid(65534)
        .gid(65534)
        .spawn()
        .unwrap();
    let peer_home = format!("/proc/{}/root{}", peer.id(), one.display());
    as_job("test -r \"$1/private\"", Path::new(&peer_home));
    let exposed = temp.path().join("runner-home");
    fs::create_dir(&exposed).unwrap();
    // Match systemd TemporaryFileSystem + BindPaths: hide the entire workspace
    // root and reveal only this job's HOME at the same host-visible pathname.
    let script = r#"
set -eu
mount --make-rprivate /
mount --bind "$1" "$2"
mount -t tmpfs -o mode=0711,nodev,nosuid tmpfs "$3"
mkdir -p "$1"
mount --bind "$2" "$1"
umount "$2"
mount -o remount,ro "$3"
exec setpriv --reuid=65534 --regid=65534 --clear-groups --no-new-privs /bin/sh -c '
    set -eu
    test "$(stat -c %u "$1")" = 65534
    echo own > "$1/own"
    ! cat "$2/private"
    ! cat "$3"
    ! cat "$4/private"
    ! sh -c '\''echo poison > "$1"'\'' probe "$3"
' probe "$1" "$4" "$5" "$6"
"#;
    assert!(
        Command::new("unshare")
            .args([
                "--user",
                "--map-users=0:0:65535",
                "--map-groups=0:0:65535",
                "--mount",
                "/bin/sh",
                "-c",
                script,
                "probe"
            ])
            .arg(&two)
            .arg(&exposed)
            .arg(temp.path().join("run"))
            .arg(&one)
            .arg(&upper)
            .arg(peer_home)
            .status()
            .unwrap()
            .success()
    );
    peer.kill().unwrap();
    peer.wait().unwrap();
    assert!(!one.join("own").exists());
    assert_eq!(fs::read(two.join("own")).unwrap(), b"own\n");
    assert_eq!(fs::read(&upper).unwrap(), b"private\n");
    cache.teardown(1).unwrap();
    cache.teardown(2).unwrap();
}

#[test]
#[ignore = "requires Linux root and overlayfs"]
fn runner_uid_change_recopies_seed_links_without_chowning_old_generations() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = linux_cache(temp.path());
    let home = cache.prepare(1, Some(key())).unwrap();
    as_job("echo warm > \"$1/cached\"", &home);
    cache.promote(1, &success(), 1).unwrap();
    let old = cache.current(&key()).unwrap();
    let original_inode = fs::metadata(old.join("cached")).unwrap().ino();
    cache.teardown(1).unwrap();
    drop(cache);
    let mut cache = Cache::open(
        settings(temp.path()),
        &temp.path().join("run"),
        Owner {
            uid: 65533,
            gid: 65533,
        },
    )
    .unwrap();
    let home = cache.prepare(2, Some(key())).unwrap();
    assert!(
        Command::new("/bin/sh")
            .args([
                "-c",
                "test -r \"$1/cached\" && echo new > \"$1/new\"",
                "probe"
            ])
            .arg(home)
            .uid(65533)
            .gid(65533)
            .status()
            .unwrap()
            .success()
    );
    cache.promote(2, &success(), 2).unwrap();
    let current = cache.current(&key()).unwrap();
    assert_eq!(fs::metadata(current.join("cached")).unwrap().uid(), 65533);
    assert_eq!(fs::metadata(old.join("cached")).unwrap().uid(), 65534);
    assert_eq!(
        fs::metadata(old.join("cached")).unwrap().ino(),
        original_inode
    );
    assert_ne!(
        fs::metadata(current.join("cached")).unwrap().ino(),
        original_inode
    );
    cache.teardown(2).unwrap();
    drop(cache);
    // Copy fallback leaves unchanged files hardlink-seeded in the candidate;
    // own_tree must re-copy them before applying yet another runner UID.
    let mut cache = Cache::copy(
        settings(temp.path()),
        &temp.path().join("run"),
        Owner {
            uid: 65532,
            gid: 65532,
        },
    )
    .unwrap();
    cache.prepare(3, Some(key())).unwrap();
    cache.promote(3, &success(), 3).unwrap();
    assert_eq!(
        fs::metadata(cache.current(&key()).unwrap().join("cached"))
            .unwrap()
            .uid(),
        65532
    );
    assert_eq!(fs::metadata(current.join("cached")).unwrap().uid(), 65533);
    cache.teardown(3).unwrap();
}

#[test]
#[ignore = "requires Linux root and overlayfs"]
fn corrupt_lease_still_pins_lazy_detached_lower_via_private_reference() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = linux_cache(temp.path());
    let home = cache.prepare(1, Some(key())).unwrap();
    let lower = cache.current(&key()).unwrap();
    let mut child = Command::new("/bin/sleep")
        .arg("30")
        .current_dir(&home)
        .spawn()
        .unwrap();
    assert!(matches!(
        cache.teardown(1),
        Err(runwell_workspace::Error::Detached)
    ));
    fs::write(temp.path().join("run/state/1.json"), b"corrupt").unwrap();
    for id in 2..=4 {
        cache.prepare(id, Some(key())).unwrap();
        cache.promote(id, &success(), id).unwrap();
        cache.teardown(id).unwrap();
    }
    cache.gc().unwrap();
    assert!(
        lower.exists(),
        "the private base marker must pin a detached lower even without a readable lease"
    );
    child.kill().unwrap();
    child.wait().unwrap();
}
