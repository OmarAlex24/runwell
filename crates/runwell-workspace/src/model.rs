use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Cache identity. Repository names are normalized case-insensitively.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheKey {
    pub(crate) repo: String,
    pub(crate) class: Option<String>,
}
impl CacheKey {
    /// Construct a repository key, optionally partitioned by job class.
    pub fn new(repo: &str, class: Option<&str>) -> Result<Self, crate::Error> {
        if !runwell_config::valid_repository(repo) || class.is_some_and(str::is_empty) {
            return Err(crate::Error::Invalid);
        }
        Ok(Self {
            repo: repo.to_ascii_lowercase(),
            class: class.map(str::to_owned),
        })
    }
    pub(crate) fn directory(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(self.repo.as_bytes());
        hash.update([0]);
        if let Some(class) = &self.class {
            hash.update(class.as_bytes());
        }
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}
/// Job account ownership, applied to private directories and promoted contents.
#[derive(Debug, Clone, Copy)]
pub struct Owner {
    /// Unprivileged user ID.
    pub uid: u32,
    /// Primary group ID.
    pub gid: u32,
}
/// Backend selected once at startup by an actual mount probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// Linux host-namespace overlayfs.
    Overlay,
    /// Portable independent writable copy.
    Copy,
}
/// Authoritative completion, obtained by the controller, never read from HOME.
#[derive(Debug, Clone)]
pub struct Completion {
    /// Actual repository assigned to this runner.
    pub repository: String,
    /// GitHub JobCompleted explicitly reported success.
    pub succeeded: bool,
    /// Authenticated workflow-run event, e.g. push or pull_request.
    pub event: String,
    /// Authenticated workflow-run head branch.
    pub branch: String,
    /// Repository's authenticated default branch.
    pub default_branch: String,
    /// Head repository matches the target (fork PRs are never trusted).
    pub same_repository: bool,
}
impl Completion {
    pub(crate) fn trusted(&self, key: &CacheKey, prs: bool) -> bool {
        self.succeeded
            && self.same_repository
            && self.repository.eq_ignore_ascii_case(&key.repo)
            && ((self.event == "push"
                && !self.default_branch.is_empty()
                && self.branch == self.default_branch)
                || (prs && self.event == "pull_request"))
    }
}
/// Result of best-effort cache promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Promotion {
    /// Published an immutable generation.
    Published(PathBuf),
    /// Missing success/trust evidence, cold unbound workspace, or wrong repository.
    Untrusted,
    /// A generation was promoted too recently, or this job was already harvested.
    Interval,
    /// The complete candidate exceeded the configured size budget.
    TooLarge,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Lease {
    pub id: u64,
    pub ready: bool,
    #[serde(default)]
    pub execution: Option<Execution>,
    pub key: Option<CacheKey>,
    pub lower: PathBuf,
    pub mode: Mode,
    pub detached_boot: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Generation {
    pub promoted_at: Option<u64>,
    pub job: Option<u64>,
}

/// Authenticated actual execution bound by a controller event, independent of
/// the request that originally provisioned the runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Execution {
    /// Actual GitHub runner request ID.
    pub request_id: i64,
    /// Actual workflow run ID.
    pub workflow_run_id: i64,
    /// Actual owner/repository.
    pub repository: String,
}
