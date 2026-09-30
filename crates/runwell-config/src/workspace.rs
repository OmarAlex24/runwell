use serde::Deserialize;
use std::{collections::BTreeMap, path::PathBuf};

/// Warm, isolated HOME cache policy. Excludes always extend the mandatory defaults.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Immutable generations; defaults to a sibling of runners_dir named caches.
    pub cache_root: Option<PathBuf>,
    /// Include job class in the repository cache key.
    pub per_class: bool,
    /// Minimum seconds between successful promotions of one key.
    pub promotion_interval_seconds: u64,
    /// Maximum logical bytes in one generation (including hardlinked files).
    pub max_generation_bytes: u64,
    /// Number of newest generations retained in addition to any live references.
    pub keep_generations: usize,
    /// Permit successful same-repository pull requests to populate caches.
    pub allow_pull_requests: bool,
    /// Additional case-insensitive path globs; `*` matches any characters.
    pub excludes: Vec<String>,
    /// Repository routing by class for organization-scoped scale sets. Only use
    /// classes whose GitHub runner-group access is restricted to that repository.
    pub repositories: BTreeMap<String, String>,
}
impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            cache_root: None,
            per_class: false,
            promotion_interval_seconds: 6 * 60 * 60,
            max_generation_bytes: 20 * 1024 * 1024 * 1024,
            keep_generations: 2,
            allow_pull_requests: false,
            excludes: Vec::new(),
            repositories: BTreeMap::new(),
        }
    }
}
impl WorkspaceConfig {
    /// Check paths and resource bounds without filesystem access.
    pub fn validate(&self) -> Result<(), super::Error> {
        let valid_path = self.cache_root.as_ref().is_none_or(|p| {
            p.is_absolute()
                && p.parent().is_some()
                && p.components()
                    .all(|c| !matches!(c, std::path::Component::ParentDir))
                && !p.to_string_lossy().contains([',', ':', '\\', '\n'])
        });
        if !valid_path
            || self.keep_generations == 0
            || self.max_generation_bytes == 0
            || self
                .excludes
                .iter()
                .any(|p| p.is_empty() || p.starts_with('/') || p.contains(".."))
            || self.repositories.values().any(|r| !valid_repository(r))
        {
            return Err(super::Error::Validation(
                "invalid workspace cache settings".into(),
            ));
        }
        Ok(())
    }
}
/// Validate an owner/repository identifier before using it in a cache key or URL.
pub fn valid_repository(repo: &str) -> bool {
    let parts: Vec<_> = repo.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        })
}
