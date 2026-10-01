use crate::Error;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;

/// Trusted upstream release metadata; Debug deliberately omits the release body.
#[derive(Clone, Deserialize)]
pub struct Release {
    /// Upstream tag, beginning with v.
    pub tag_name: String,
    /// UTC publication timestamp.
    pub published_at: jiff::Timestamp,
    /// Published checksums, never used as executable instructions.
    pub body: String,
    /// Pre-releases are excluded from automatic upgrades.
    #[serde(default)]
    pub prerelease: bool,
    /// Draft releases are excluded from automatic upgrades.
    #[serde(default)]
    pub draft: bool,
}
impl Release {
    /// Numeric runner version, validated before constructing a path or URL.
    pub fn version(&self) -> Result<&str, Error> {
        let v = self.tag_name.strip_prefix('v').ok_or(Error::Release)?;
        version_parts(v)?;
        Ok(v)
    }
}
fn version_parts(value: &str) -> Result<Vec<u64>, Error> {
    let parts: Vec<u64> = value
        .split('.')
        .map(|v| v.parse().map_err(|_| Error::Release))
        .collect::<Result<_, _>>()?;
    if parts.len() == 3 {
        Ok(parts)
    } else {
        Err(Error::Release)
    }
}
/// Freshness relative to the oldest newer published release, not installation age.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseStatus {
    /// Current or fewer than 21 days behind a newer release.
    Current,
    /// At least 21 days behind.
    Warn,
    /// At least 25 days behind: promote latest for new jobs.
    Refresh,
    /// At least 30 days behind: fail closed until refreshed.
    Expired,
}
/// Compute update policy from release publication timestamps. A newer release
/// cannot reset the deadline already imposed by an earlier release.
pub fn release_status(
    current: &str,
    releases: &[Release],
    now: jiff::Timestamp,
) -> Result<ReleaseStatus, Error> {
    let oldest = oldest_newer(current, releases)?;
    let days = oldest.map_or(0, |t| (now.as_second() - t.as_second()).max(0) / 86400);
    Ok(match days {
        0..=20 => ReleaseStatus::Current,
        21..=24 => ReleaseStatus::Warn,
        25..=29 => ReleaseStatus::Refresh,
        _ => ReleaseStatus::Expired,
    })
}
/// Enforced expiry seconds, measured from the oldest newer stable release.
pub fn release_deadline(current: &str, releases: &[Release]) -> Result<Option<i64>, Error> {
    Ok(oldest_newer(current, releases)?.map(|at| at.as_second().saturating_add(30 * 86400)))
}
fn oldest_newer(current: &str, releases: &[Release]) -> Result<Option<jiff::Timestamp>, Error> {
    let current = version_parts(current)?;
    let mut oldest = None;
    for release in releases.iter().filter(|r| !r.draft && !r.prerelease) {
        if version_parts(release.version()?)? > current {
            oldest = Some(oldest.map_or(release.published_at, |v: jiff::Timestamp| {
                v.min(release.published_at)
            }));
        }
    }
    Ok(oldest)
}
/// Require either a pinned digest or a checksum associated with this exact asset.
/// Both GitHub's Markdown table and sha256sum formats are supported; ambiguity
/// or a digest from another platform fails closed.
pub fn checksum(version: &str, body: &str, pinned: Option<&str>) -> Result<[u8; 32], Error> {
    version_parts(version)?;
    if let Some(value) = pinned {
        return hex_digest(value);
    }
    let asset = format!("actions-runner-linux-x64-{version}.tar.gz");
    let mut found = None;
    for line in body.lines().filter(|l| l.contains(&asset)) {
        for word in line.split(|c: char| !c.is_ascii_hexdigit()) {
            if word.len() == 64 {
                let hash = hex_digest(word)?;
                if found.is_some_and(|previous| previous != hash) {
                    return Err(Error::Checksum);
                }
                found = Some(hash);
            }
        }
    }
    found.ok_or(Error::Checksum)
}
fn hex_digest(value: &str) -> Result<[u8; 32], Error> {
    if value.len() != 64 || !value.is_ascii() {
        return Err(Error::Checksum);
    }
    let mut bytes = [0; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).map_err(|_| Error::Checksum)?;
    }
    Ok(bytes)
}
/// Public GitHub release client. No administrative credential is sent here.
pub struct ReleaseClient {
    client: reqwest::Client,
}
impl ReleaseClient {
    /// Build a client with bounded request and download times.
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent("runwell-runner")
                .connect_timeout(Duration::from_secs(30))
                .timeout(Duration::from_secs(600))
                .build()
                .map_err(|_| Error::Network)?,
        })
    }
    /// Read the pinned release's published checksum metadata.
    pub async fn release(&self, version: &str) -> Result<Release, Error> {
        version_parts(version)?;
        let release: Release = self
            .client
            .get(format!(
                "https://api.github.com/repos/actions/runner/releases/tags/v{version}"
            ))
            .send()
            .await
            .map_err(|_| Error::Network)?
            .error_for_status()
            .map_err(|_| Error::Network)?
            .json()
            .await
            .map_err(|_| Error::Release)?;
        if release.version()? != version || release.draft || release.prerelease {
            return Err(Error::Release);
        }
        Ok(release)
    }
    /// Recent stable releases. One hundred releases is conservative: if the pinned
    /// version is older, the oldest newer timestamp still imposes a deadline.
    pub async fn releases(&self) -> Result<Vec<Release>, Error> {
        self.client
            .get("https://api.github.com/repos/actions/runner/releases?per_page=100")
            .send()
            .await
            .map_err(|_| Error::Network)?
            .error_for_status()
            .map_err(|_| Error::Network)?
            .json()
            .await
            .map_err(|_| Error::Release)
    }
    /// Download with a two-GiB bound and verify before any extraction occurs.
    pub async fn download(
        &self,
        version: &str,
        expected: [u8; 32],
        destination: &Path,
    ) -> Result<(), Error> {
        version_parts(version)?;
        let mut response = self.client.get(format!("https://github.com/actions/runner/releases/download/v{version}/actions-runner-linux-x64-{version}.tar.gz"))
            .send().await.map_err(|_| Error::Network)?.error_for_status().map_err(|_| Error::Network)?;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .await?;
        let mut hasher = Sha256::new();
        let mut length = 0_u64;
        while let Some(bytes) = response.chunk().await.map_err(|_| Error::Network)? {
            length = length.saturating_add(bytes.len() as u64);
            if length > 2 * 1024 * 1024 * 1024 {
                return Err(Error::Release);
            }
            hasher.update(&bytes);
            file.write_all(&bytes).await?;
        }
        file.sync_all().await?;
        let digest: [u8; 32] = hasher.finalize().into();
        if digest != expected {
            return Err(Error::Checksum);
        }
        Ok(())
    }
}
