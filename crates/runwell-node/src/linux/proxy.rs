use super::LinuxBackend;
use crate::{Error, SliceSpec};
use runwell_dockerproxy::{ProxyManager, ProxySpec};

impl LinuxBackend {
    /// Enable Docker attribution for production node execution. Kept separate
    /// from connect so systemd-only host probes do not require a Docker daemon.
    pub fn with_docker_proxy(mut self, node: String) -> Self {
        self.docker_proxy = Some(ProxyManager::new(self.settings.docker_proxy.clone(), node));
        self
    }
    pub(super) async fn proxy_environment(&self, slice: &SliceSpec) -> Result<Vec<String>, Error> {
        let Some(proxy) = &self.docker_proxy else {
            return Ok(Vec::new());
        };
        // A restart can load changed class settings. Use the surviving slice's
        // actual ceiling, not a newly configured limit that was never applied.
        let ceiling = std::fs::read_to_string(format!(
            "/sys/fs/cgroup/ci.slice/ci-rw.slice/{}/memory.max",
            slice.unit()
        ))?;
        let memory_max = ceiling.trim().parse::<u64>().map_err(|_| Error::Config)?;
        Ok(proxy
            .ensure(ProxySpec {
                job_id: slice.job_id,
                node: String::new(), // Manager supplies the authenticated node identity.
                cgroup_parent: slice.unit(),
                memory_max,
                uid: self.user.uid,
                gid: self.user.gid,
            })
            .await?)
    }
    pub(super) async fn cleanup_proxy(&self, id: u64) -> Result<(), Error> {
        if let Some(proxy) = &self.docker_proxy {
            proxy.cleanup(id).await?;
        }
        Ok(())
    }
}
