use crate::Error;
use secrecy::{ExposeSecret, SecretString};
use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
};

fn path(id: u64) -> PathBuf {
    PathBuf::from("/run/runwell").join(format!("j{id}.env"))
}
// Environment= is exposed by systemd's D-Bus properties and transient unit text.
// EnvironmentFiles= exposes only this path. PID 1 reads the root-only tmpfs file
// before switching User=; the listener still receives JIT via its environment.
pub(super) fn write(id: u64, secret: &SecretString) -> Result<String, Error> {
    let value = secret.expose_secret();
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=_-".contains(&b))
    {
        return Err(Error::Config);
    }
    let root = std::path::Path::new("/run/runwell");
    fs::create_dir_all(root)?;
    if fs::symlink_metadata(root)?.is_symlink() || fs::metadata(root)?.uid() != 0 {
        return Err(Error::Config);
    }
    // Jobs traverse this root to reach their Docker socket. Credential files
    // remain root-only (0600), and the directory cannot be listed by runners.
    fs::set_permissions(root, fs::Permissions::from_mode(0o711))?;
    use rustix::fs::{Mode, OFlags, open};
    let fd = open(
        path(id),
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|_| Error::Io)?;
    let mut file = fs::File::from(fd);
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(b"ACTIONS_RUNNER_INPUT_JITCONFIG=")?;
    file.write_all(value.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(path(id).to_string_lossy().into_owned())
}
pub(super) fn remove(id: u64) -> Result<(), Error> {
    match fs::remove_file(path(id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Error::Io),
    }
}
