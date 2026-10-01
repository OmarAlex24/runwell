use crate::Fleet;
use runwell_node::Error;
use runwell_transport::{Report, Request};

impl Fleet {
    pub(crate) async fn orphans(&self, reports: &[(String, i64, Report)]) -> Result<(), Error> {
        let placements = self.store.placements().await?;
        let runners = self.store.runners().await?;
        let controller = &self
            .config
            .network
            .as_ref()
            .ok_or(Error::Config)?
            .controller_id;
        for (id, _, report) in reports {
            for local in &report.inventory {
                let owned = placements
                    .iter()
                    .any(|p| p.job_id == local.id as i64 && &p.node_id == id);
                let cleaned = runners
                    .iter()
                    .any(|r| r.job_id == local.id as i64 && r.cleaned);
                if owned && !cleaned {
                    continue;
                }
                let name = format!("rw-{controller}-j{}", local.id);
                if let Some(remote) = self.api.lookup(&name).await?
                    && (remote.name != name || !self.api.delete(remote.id).await?)
                {
                    continue;
                }
                if let Some(peer) = self.peers.get(id) {
                    let _ = peer
                        .call(Request::Orphan(runwell_transport::Key {
                            job_id: local.id as i64,
                            attempt: report
                                .jobs
                                .iter()
                                .find(|j| j.key.job_id == local.id as i64)
                                .map_or(1, |j| j.key.attempt),
                        }))
                        .await;
                }
            }
        }
        Ok(())
    }
}
