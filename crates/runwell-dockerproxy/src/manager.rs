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
        self.settings.validate().map_err(|_| Error::Config)?;
        spec.node.clone_from(&self.node);
        if self.resources.get().is_some() {
            tokio::net::UnixStream::connect(&self.settings.upstream_socket)
                .await
                .map_err(|_| Error::Upstream)?;
        }
        let driver = self.resources().await?.driver();
        let mut jobs = self.jobs.lock().await;
        if let std::collections::btree_map::Entry::Vacant(entry) = jobs.entry(spec.job_id) {
            let rewrite = Rewriter::new(spec.clone(), self.settings.clone(), driver)?;
            entry.insert(Proxy::start(rewrite).await?);
        }
        drop(jobs);
        Ok(spec.environment(&self.settings))
    }
    /// Start attribution lazily when Docker is available. Non-Docker jobs can
    /// still run on hosts without a daemon; configuration failures remain fatal.
    pub async fn prepare(&self, spec: ProxySpec) -> Result<Vec<String>, Error> {
        let id = spec.job_id;
        match self.ensure(spec).await {
            Err(Error::Upstream) => {
                tracing::warn!(
                    job_id = id,
                    "Docker unavailable; starting runner without Docker proxy environment"
                );
                Ok(Vec::new())
            }
            result => result,
        }
    }
    /// Quiesce the proxy before removing its resources; retain runtime directories
    /// until successful cleanup so a crash still leaves an inventory identity.
    pub async fn cleanup(&self, id: u64) -> Result<(), Error> {
        let directory = self.settings.run_dir.join("jobs").join(id.to_string());
        let proxy = self.jobs.lock().await.remove(&id);
        let used_docker = proxy.is_some() || directory.try_exists()?;
        let stopped = if let Some(proxy) = proxy {
            proxy.shutdown().await
        } else {
            Ok(())
        };
        // A job without a proxy identity may have run on a Docker-less host.
        // When Docker is up we still inspect labels for legacy/label-only jobs;
        // when it is down, later inventory will rediscover label-only orphans.
        let cleaned = if !used_docker
            && tokio::net::UnixStream::connect(&self.settings.upstream_socket)
                .await
                .is_err()
        {
            Ok(())
        } else {
            // Do not mistake failed Docker API removals for an absent daemon.
            // Label-only orphans still need retries even without a runtime path.
            match self.resources().await {
                Ok(resources) => resources.cleanup(id).await,
                Err(error) => Err(error),
            }
        };
        stopped.and(cleaned)?;
        crate::socket::remove(&directory.join("docker.sock"))?;
        match fs::remove_dir(directory) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(Error::Io),
        }
    }
    /// Combine daemon labels with runtime identities for M3 orphan reconciliation.
    pub async fn inventory(&self) -> Result<BTreeSet<u64>, Error> {
        let remote = match self.resources().await {
            Ok(resources) => resources.inventory().await,
            Err(error) => Err(error),
        };
        let mut ids = match remote {
            Ok(ids) => ids,
            Err(Error::Upstream) => {
                tracing::warn!(
                    "Docker unavailable during inventory; retaining local proxy identities"
                );
                BTreeSet::new()
            }
            Err(error) => return Err(error),
        };
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
