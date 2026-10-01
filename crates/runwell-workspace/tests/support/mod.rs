#![allow(dead_code)]
use runwell_config::WorkspaceConfig;
use runwell_workspace::{Cache, CacheKey, Completion, Owner};
use std::{fs, path::Path};

pub fn owner() -> Owner {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // A freshly-created file belongs to the current test user (unlike /tmp).
        let dir = tempfile::tempdir().unwrap();
        let user = fs::metadata(dir.path()).unwrap();
        Owner {
            uid: user.uid(),
            gid: user.gid(),
        }
    }
    #[cfg(not(unix))]
    {
        Owner { uid: 0, gid: 0 }
    }
}
pub fn settings(root: &Path) -> WorkspaceConfig {
    WorkspaceConfig {
        cache_root: Some(root.join("cache")),
        promotion_interval_seconds: 0,
        ..Default::default()
    }
}
pub fn cache(root: &Path) -> Cache {
    Cache::copy(settings(root), &root.join("run"), owner()).unwrap()
}
pub fn key() -> CacheKey {
    CacheKey::new("example/repo", None).unwrap()
}
pub fn success() -> Completion {
    Completion {
        repository: "example/repo".into(),
        succeeded: true,
        event: "push".into(),
        branch: "main".into(),
        default_branch: "main".into(),
        same_repository: true,
    }
}
pub fn put(path: &Path, contents: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}
