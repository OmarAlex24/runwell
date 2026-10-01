use crate::Fleet;
use runwell_node::Error;
use runwell_runner::{ReleaseStatus, release_status};

impl Fleet {
    pub(crate) async fn release_version(&self, force: bool) -> Result<String, Error> {
        let configured = &self
            .config
            .standalone
            .as_ref()
            .ok_or(Error::Config)?
            .runner
            .version;
        let Some(releases) = &self.releases else {
            return Ok(configured.clone());
        };
        let client = releases.lock().await;
        let now = self.clock.now_ms();
        let saved = self.store.release().await?;
        let (mut version, checked) = saved.unwrap_or_else(|| (configured.clone(), 0));
        if force || now.saturating_sub(checked) >= 86_400_000 {
            let releases = client.releases().await?;
            let status = release_status(
                &version,
                &releases,
                jiff::Timestamp::from_millisecond(now).map_err(|_| Error::Config)?,
            )?;
            if status == ReleaseStatus::Warn {
                tracing::warn!("runner release is at least 21 days behind");
            }
            if force || matches!(status, ReleaseStatus::Refresh | ReleaseStatus::Expired) {
                let latest = releases
                    .iter()
                    .filter(|r| !r.draft && !r.prerelease)
                    .max_by_key(|r| r.published_at)
                    .ok_or(Error::Config)?;
                version = latest.version()?.into();
                // Refuse to select a release lacking published checksum evidence.
                runwell_runner::checksum(&version, &latest.body, None)?;
            }
            self.store.select_release(version.clone(), now).await?;
        }
        Ok(version)
    }
}
