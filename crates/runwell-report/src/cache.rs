//! Private raw-response disk cache; credentials never enter cache keys or files.
use crate::Error;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize)]
struct Entry {
    key: String,
    fetched: u64,
    body: String,
}

pub struct Cache {
    root: PathBuf,
}
impl Cache {
    pub fn new(root: PathBuf) -> Result<Self, Error> {
        std::fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self { root })
    }
    fn path(&self, key: &str) -> PathBuf {
        let hash = key.bytes().fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x100000001b3)
        });
        self.root.join(format!("{hash:016x}.json"))
    }
    pub fn get(&self, key: &str, immutable: bool) -> Result<Option<String>, Error> {
        let text = match std::fs::read_to_string(self.path(key)) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        // Corrupt entries are cache misses, never substitutes for fresh API responses.
        let Ok(entry) = serde_json::from_str::<Entry>(&text) else {
            return Ok(None);
        };
        Ok(
            (entry.key == key && (immutable || now().saturating_sub(entry.fetched) < 300))
                .then_some(entry.body),
        )
    }
    pub fn put(&self, key: &str, body: String) -> Result<(), Error> {
        let path = self.path(key);
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        let data = serde_json::to_vec(&Entry {
            key: key.into(),
            fetched: now(),
            body,
        })?;
        write_private(&tmp, &data)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn write_private(path: &Path, data: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(data)?;
    Ok(())
}
