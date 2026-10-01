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
        let memory_max = ProxySpec::parse_memory_max(&ceiling)?;
        Ok(proxy
            .prepare(ProxySpec {
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
    pub(super) async fn teardown_job(&self, id: u64, keep_slice: bool) -> Result<(), Error> {
        let stopped = self.systemd.stop(&crate::service_unit(id)).await;
        let docker = self.cleanup_proxy(id).await;
        let result = docker.and(stopped);
        if keep_slice && result.is_ok() {
            return result;
        }
        // Docker availability must never gate stopping local units or deleting
        // credentials/installations. Reconcile keeps the proxy runtime identity.
        let slice = self.systemd.stop(&crate::slice_unit(id)).await;
        let quiescent = result.is_ok() && slice.is_ok();
        let steps: [crate::NodeFuture<'_, ()>; 3] = [
            Box::pin(async { super::credentials::remove(id) }),
            // Retain upper and mount leases until every writer is gone.
            Box::pin(async {
                if quiescent {
                    self.workspaces.teardown(id).await?;
                }
                Ok(())
            }),
            Box::pin(async {
                runwell_runner::remove_install(
                    &self.settings.runners_dir,
                    &self.settings.runners_dir.join(format!("j{id}")),
                )?;
                Ok(())
            }),
        ];
        crate::teardown::finish(result.and(slice), steps).await
    }
}
