use crate::{CgroupDriver, DockerProxyConfig, Error};
use bollard::{Docker, query_parameters::*};
use std::collections::{BTreeSet, HashMap};

/// Upstream-only control client. Every destructive action requires both labels.
pub struct DockerResources {
    docker: Docker,
    node: String,
    stop_seconds: u32,
    driver: CgroupDriver,
}
impl DockerResources {
    /// Negotiate API compatibility and detect the cgroup driver once.
    pub async fn connect(settings: &DockerProxyConfig, node: String) -> Result<Self, Error> {
        let docker = Docker::connect_with_unix(
            settings.upstream_socket.to_str().ok_or(Error::Config)?,
            u64::from(settings.stop_seconds) + 30,
            bollard::API_DEFAULT_VERSION,
        )
        .map_err(|_| Error::Upstream)?
        .negotiate_version()
        .await
        .map_err(|_| Error::Upstream)?;
        let info = docker.info().await.map_err(|_| Error::Upstream)?;
        let driver = CgroupDriver::parse(info.cgroup_driver.ok_or(Error::Config)?.as_ref())?;
        Ok(Self {
            docker,
            node,
            stop_seconds: settings.stop_seconds,
            driver,
        })
    }
    /// Driver cached at connection time.
    pub fn driver(&self) -> CgroupDriver {
        self.driver
    }
    fn filters(&self, id: Option<u64>) -> Option<HashMap<String, Vec<String>>> {
        let mut labels = vec![format!("io.runwell.node={}", self.node)];
        labels.push(id.map_or_else(
            || "io.runwell.job".into(),
            |id| format!("io.runwell.job={id}"),
        ));
        Some(HashMap::from([("label".into(), labels)]))
    }
    fn job(&self, labels: &HashMap<String, String>) -> Option<u64> {
        if labels.get("io.runwell.node")? != &self.node {
            return None;
        }
        let value = labels.get("io.runwell.job")?;
        value
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0 && value == &id.to_string())
    }
    async fn objects(&self, id: Option<u64>) -> Result<Objects, Error> {
        let filters = self.filters(id);
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: filters.clone(),
                ..Default::default()
            }))
            .await
            .map_err(|_| Error::Upstream)?;
        let networks = self
            .docker
            .list_networks(Some(ListNetworksOptions {
                filters: filters.clone(),
            }))
            .await
            .map_err(|_| Error::Upstream)?;
        let volumes = self
            .docker
            .list_volumes(Some(ListVolumesOptions { filters }))
            .await
            .map_err(|_| Error::Upstream)?;
        // Recheck labels even though the daemon was asked to filter. In particular,
        // never infer ownership from a name, missing labels or an empty response.
        let owned = |labels: Option<&HashMap<String, String>>| {
            labels
                .and_then(|l| self.job(l))
                .filter(|job| id.is_none_or(|id| id == *job))
        };
        Ok(Objects {
            containers: containers
                .into_iter()
                .filter_map(|c| Some((owned(c.labels.as_ref())?, c.id?)))
                .collect(),
            networks: networks
                .into_iter()
                .filter_map(|n| Some((owned(n.labels.as_ref())?, n.id?)))
                .collect(),
            volumes: volumes
                .volumes
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| Some((owned(Some(&v.labels))?, v.name)))
                .collect(),
        })
    }
    /// Discover label-only orphans even if their units and runtime paths vanished.
    pub async fn inventory(&self) -> Result<BTreeSet<u64>, Error> {
        let objects = self.objects(None).await?;
        Ok(objects
            .containers
            .into_iter()
            .chain(objects.networks)
            .chain(objects.volumes)
            .map(|(id, _)| id)
            .collect())
    }
    /// Stop, force-remove containers, then remove networks and labeled volumes.
    /// Missing/already-stopped objects are successful, making retries idempotent.
    pub async fn cleanup(&self, job: u64) -> Result<(), Error> {
        let objects = self.objects(Some(job)).await?;
        for (_, id) in &objects.containers {
            // A failed graceful stop still proceeds to force removal. Only the
            // final removal determines whether durable cleanup can be completed.
            let _ = self
                .docker
                .stop_container(
                    id,
                    Some(StopContainerOptions {
                        t: Some(self.stop_seconds as i32),
                        ..Default::default()
                    }),
                )
                .await;
        }
        let mut failed = false;
        for (_, id) in &objects.containers {
            failed |= !removed(
                self.docker
                    .remove_container(
                        id,
                        Some(RemoveContainerOptions {
                            force: true,
                            v: false,
                            ..Default::default()
                        }),
                    )
                    .await,
            );
        }
        for (_, id) in &objects.networks {
            failed |= !removed(self.docker.remove_network(id).await);
        }
        for (_, name) in &objects.volumes {
            failed |= !removed(
                self.docker
                    .remove_volume(name, Some(RemoveVolumeOptions { force: true }))
                    .await,
            );
        }
        if failed { Err(Error::Upstream) } else { Ok(()) }
    }
}
struct Objects {
    containers: Vec<(u64, String)>,
    networks: Vec<(u64, String)>,
    volumes: Vec<(u64, String)>,
}
fn removed(result: Result<(), bollard::errors::Error>) -> bool {
    matches!(
        result,
        Ok(())
            | Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })
    )
}
