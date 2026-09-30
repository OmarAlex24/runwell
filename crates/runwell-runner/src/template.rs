use crate::{Error, ReleaseClient, Template, checksum};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, chown},
    path::Path,
};

/// Dedicated local account, resolved without invoking a shell.
#[derive(Debug, Clone)]
pub struct RunnerUser {
    /// Account name for systemd User= and archive extraction.
    pub name: String,
    /// Nonzero UNIX UID.
    pub uid: u32,
    /// Primary GID.
    pub gid: u32,
}
impl RunnerUser {
    /// Resolve an existing local account; root is forbidden.
    pub fn resolve(name: &str) -> Result<Self, Error> {
        let passwd = fs::read_to_string("/etc/passwd")?;
        for line in passwd.lines() {
            let fields: Vec<_> = line.split(':').collect();
            if fields.len() >= 4 && fields[0] == name {
                let uid = fields[2].parse().map_err(|_| Error::User)?;
                let gid = fields[3].parse().map_err(|_| Error::User)?;
                if uid == 0 || gid == 0 {
                    return Err(Error::User);
                }
                return Ok(Self {
                    name: name.into(),
                    uid,
                    gid,
                });
            }
        }
        Err(Error::User)
    }
    /// Transfer private directories and copied files, leaving immutable hardlinks
    /// owned by root so a runner cannot chmod or modify another job's binaries.
    pub fn own_install(&self, root: &Path) -> Result<(), Error> {
        own_tree(root, self, false)
    }
}
fn own_tree(path: &Path, user: &RunnerUser, immutable: bool) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_symlink() {
        return Err(Error::Release);
    }
    if metadata.is_dir() {
        chown(path, Some(user.uid), Some(user.gid))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let immutable =
                immutable || entry.file_name() == "bin" || entry.file_name() == "externals";
            own_tree(&entry.path(), user, immutable)?;
        }
    } else if !immutable {
        chown(path, Some(user.uid), Some(user.gid))?;
    }
    Ok(())
}
fn seal(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
        return Err(Error::Release);
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            seal(&entry?.path())?;
        }
    }
    chown(path, Some(0), Some(0))?;
    let mode = if metadata.is_dir() || metadata.permissions().mode() & 0o111 != 0 {
        0o555
    } else {
        0o444
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}
/// Download, verify, extract as the runner user, seal as root, then atomically
/// promote. No existing template generation is changed. A root-owned marker is
/// written only after verification and extraction have both succeeded.
pub async fn stage_template(
    client: &ReleaseClient,
    root: &Path,
    version: &str,
    pinned: Option<&str>,
    user: &RunnerUser,
) -> Result<Template, Error> {
    let release = client.release(version).await?;
    let digest = checksum(version, &release.body, pinned)?;
    let destination = root.join(version);
    let marker = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if destination.exists() {
        if fs::read_to_string(destination.join(".runwell-verified"))? != marker {
            return Err(Error::Checksum);
        }
        return Ok(Template {
            version: version.into(),
            directory: destination,
            sha256: digest,
        });
    }
    fs::create_dir_all(root)?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o755))?;
    let staging = root.join(format!(".stage-{version}"));
    let archive = root.join(format!(".download-{version}.tar.gz"));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    if archive.exists() {
        fs::remove_file(&archive)?;
    }
    client.download(version, digest, &archive).await?;
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o644))?;
    fs::create_dir(&staging)?;
    chown(&staging, Some(user.uid), Some(user.gid))?;
    let status = tokio::process::Command::new("/usr/sbin/runuser")
        .kill_on_drop(true)
        .args([
            "-u",
            &user.name,
            "--",
            "/usr/bin/tar",
            "--no-same-owner",
            "--no-same-permissions",
            "-xzf",
        ])
        .arg(&archive)
        .arg("-C")
        .arg(&staging)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    if !status.success() || !staging.join("bin/Runner.Listener").is_file() {
        return Err(Error::Extract);
    }
    fs::write(staging.join(".runwell-verified"), marker)?;
    seal(&staging)?;
    fs::rename(&staging, &destination)?;
    fs::File::open(root)?.sync_all()?;
    fs::remove_file(&archive)?;
    Ok(Template {
        version: version.into(),
        directory: destination,
        sha256: digest,
    })
}
