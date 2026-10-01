use crate::Error;
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, chown},
    path::Path,
};
use tokio::net::{UnixListener, UnixStream};

pub(crate) async fn bind(path: &Path, uid: u32, gid: u32) -> Result<UnixListener, Error> {
    let parent = path.parent().ok_or(Error::Config)?;
    // Validate each existing component; never follow a job-controlled symlink.
    let mut current = std::path::PathBuf::new();
    for component in parent.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_symlink() || !metadata.is_dir() => {
                return Err(Error::Config);
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                fs::set_permissions(&current, fs::Permissions::from_mode(0o711))?;
            }
            Err(_) => return Err(Error::Io),
        }
    }
    // The job can connect but cannot unlink/rebind its socket pathname.
    for directory in parent.ancestors().take(3) {
        let metadata = fs::metadata(directory)?;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
            return Err(Error::Config);
        }
        fs::set_permissions(directory, fs::Permissions::from_mode(0o711))?;
    }
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.file_type().is_socket() {
            return Err(Error::Config);
        }
        match UnixStream::connect(path).await {
            Ok(_) => return Err(Error::Config),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                remove(path)?
            }
            Err(_) => return Err(Error::Io),
        }
    }
    let (private, listener) = stage(parent, uid, gid)?;
    fs::rename(private.path().join("s"), path)?;
    Ok(listener)
}

// tempfile creates this directory atomically with 0700. The production node is
// root, so a job cannot reach the socket until ownership and mode are final.
fn stage(parent: &Path, uid: u32, gid: u32) -> Result<(tempfile::TempDir, UnixListener), Error> {
    let private = tempfile::Builder::new()
        .prefix(".p")
        .rand_bytes(3)
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)?;
    let path = private.path().join("s");
    let listener = UnixListener::bind(&path)?;
    let metadata = fs::metadata(&path)?;
    if metadata.uid() != uid || metadata.gid() != gid {
        chown(&path, Some(uid), Some(gid))?;
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o660))?;
    Ok((private, listener))
}

pub(crate) fn remove(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            fs::remove_file(path).map_err(Error::from)
        }
        Ok(_) => Err(Error::Config),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Error::Io),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn socket_is_private_until_final_permissions_are_published() {
        let root = tempfile::tempdir().unwrap();
        let uid = rustix::process::geteuid().as_raw();
        let gid = rustix::process::getegid().as_raw();
        let target = root.path().join("docker.sock");
        let (private, listener) = stage(root.path(), uid, gid).unwrap();
        let directory = fs::metadata(private.path()).unwrap();
        assert_eq!(directory.mode() & 0o777, 0o700);
        assert_eq!(directory.uid(), uid);
        assert!(!target.exists());
        let socket = private.path().join("s");
        let metadata = fs::metadata(&socket).unwrap();
        assert_eq!(
            (metadata.uid(), metadata.gid(), metadata.mode() & 0o777),
            (uid, gid, 0o660)
        );
        fs::rename(socket, &target).unwrap();
        drop(private);
        let _client = UnixStream::connect(&target).await.unwrap();
        listener.accept().await.unwrap();
        assert_eq!(fs::metadata(target).unwrap().mode() & 0o777, 0o660);
    }
}
