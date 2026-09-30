//! Local state only. Private permissions and atomic replacement protect resumes.
use crate::{Error, model::SetupSession};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub fn state_path() -> Result<PathBuf, Error> {
    dirs::config_dir()
        .map(|dir| dir.join("runwell").join("setup-session.json"))
        .ok_or_else(|| {
            Error::Usage("platform config directory unavailable; provide --state-file".into())
        })
}

pub fn load(path: &Path) -> Result<SetupSession, Error> {
    match fs::read(path) {
        Ok(data) => Ok(serde_json::from_slice(&data)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SetupSession::default()),
        Err(error) => Err(error.into()),
    }
}

pub fn save(path: &Path, session: &SetupSession) -> Result<(), Error> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".setup-{}.json", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        serde_json::to_writer_pretty(&mut file, session)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok::<_, Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
