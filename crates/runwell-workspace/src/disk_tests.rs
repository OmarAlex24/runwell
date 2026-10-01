use crate::disk;
use std::fs;

#[cfg(unix)]
#[test]
fn opening_job_files_rejects_swapped_symlinks_and_fifos_without_blocking() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::write(&source, b"warm").unwrap();
    fs::write(&target, b"keep").unwrap();
    assert!(fs::symlink_metadata(&source).unwrap().is_file());
    fs::remove_file(&source).unwrap();
    std::os::unix::fs::symlink(&target, &source).unwrap();
    assert!(disk::copy_file(&source, &target).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"keep");
    fs::remove_file(&source).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    let started = std::time::Instant::now();
    assert!(disk::copy_file(&source, &target).is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(fs::read(&target).unwrap(), b"keep");
    assert!(disk::open_regular(temp.path()).is_err());
}

#[test]
fn reads_the_opened_inode_even_after_the_path_is_replaced() {
    use std::io::Read;
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::write(&source, b"original").unwrap();
    let mut file = disk::open_regular(&source).unwrap();
    fs::rename(&source, temp.path().join("old")).unwrap();
    fs::write(&source, b"replacement").unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"original");
}
