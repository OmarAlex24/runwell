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
    let listener = UnixListener::bind(path)?;
    let permissions = (|| {
        let metadata = fs::metadata(path)?;
        if metadata.uid() != uid || metadata.gid() != gid {
            chown(path, Some(uid), Some(gid))?;
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o660))
    })();
    if permissions.is_err() {
        let _ = remove(path);
        return Err(Error::Io);
    }
    Ok(listener)
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
