use crate::{DockerProxyConfig, DockerResources, Error, Proxy, ProxySpec, Rewriter};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};
use tokio::sync::{Mutex, OnceCell};

/// Small node lifecycle boundary: restart sockets, produce env, clean resources.
/// The node creates slices first and calls cleanup before final measurement.
pub struct ProxyManager {
    settings: DockerProxyConfig,
    node: String,
    resources: OnceCell<DockerResources>,
    jobs: Mutex<BTreeMap<u64, Proxy>>,
}
impl ProxyManager {
    /// Build without contacting Docker; connection/driver detection is lazy.
    pub fn new(settings: DockerProxyConfig, node: String) -> Self {
        Self {
            settings,
            node,
            resources: OnceCell::new(),
            jobs: Mutex::new(BTreeMap::new()),
        }
    }
    async fn resources(&self) -> Result<&DockerResources, Error> {
        self.resources
            .get_or_try_init(|| DockerResources::connect(&self.settings, self.node.clone()))
            .await
    }
    /// Ensure exactly one proxy for a job and return the runner environment.
    /// The same call reopens a stale socket during node restart reconciliation.
    pub async fn ensure(&self, mut spec: ProxySpec) -> Result<Vec<String>, Error> {
        spec.node.clone_from(&self.node);
        let mut jobs = self.jobs.lock().await;
        if let std::collections::btree_map::Entry::Vacant(entry) = jobs.entry(spec.job_id) {
            let driver = self.resources().await?.driver();
            let rewrite = Rewriter::new(spec.clone(), self.settings.clone(), driver)?;
            entry.insert(Proxy::start(rewrite).await?);
        }
        Ok(spec.environment(&self.settings))
    }
    /// Quiesce the proxy before removing its resources; retain runtime directories
    /// until successful cleanup so a crash still leaves an inventory identity.
    pub async fn cleanup(&self, id: u64) -> Result<(), Error> {
        let mut jobs = self.jobs.lock().await;
        if let Some(proxy) = jobs.remove(&id) {
            proxy.shutdown().await?;
        }
        self.resources().await?.cleanup(id).await?;
        let directory = self.settings.run_dir.join("jobs").join(id.to_string());
        crate::socket::remove(&directory.join("docker.sock"))?;
        match fs::remove_dir(directory) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(Error::Io),
        }
    }
    /// Combine daemon labels with runtime identities for M3 orphan reconciliation.
    pub async fn inventory(&self) -> Result<BTreeSet<u64>, Error> {
        let mut ids = self.resources().await?.inventory().await?;
        match fs::read_dir(self.settings.run_dir.join("jobs")) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if let Some(id) = entry
                        .file_name()
                        .to_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .filter(|id| *id > 0)
                    {
                        ids.insert(id);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::Io),
        }
        Ok(ids)
    }
}
