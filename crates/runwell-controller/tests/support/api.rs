use super::*;
use runwell_node::{Error, Registration, RunnerApi};
pub struct Api {
    pub store: Store,
    pub nodes: Vec<Store>,
    pub runners: Mutex<BTreeMap<i64, String>>,
    pub creates: AtomicUsize,
    pub busy: AtomicBool,
    pub lose_create: AtomicBool,
}
impl RunnerApi for Api {
    fn acquire(&self, _set: i64, request: i64) -> NodeFuture<'_, bool> {
        Box::pin(async move {
            let mut admitted = false;
            for store in &self.nodes {
                admitted |= store.active_leases().await?.iter().any(|l| {
                    l.job.metadata.request_id == request && l.phase >= runwell_transport::PREPARED
                });
            }
            assert!(admitted, "acquisition before host admission/preparation");
            Ok(true)
        })
    }
    fn create<'a>(&'a self, _set: i64, name: &'a str) -> NodeFuture<'a, Registration> {
        Box::pin(async move {
            let runner = self
                .store
                .runners()
                .await?
                .into_iter()
                .find(|r| r.name == name)
                .unwrap();
            assert!(self.store.job(runner.job_id).await?.acquired);
            self.creates.fetch_add(1, Ordering::SeqCst);
            let id = 1000 + runner.job_id;
            assert!(
                self.runners.lock().await.insert(id, name.into()).is_none(),
                "runner created twice"
            );
            if self.lose_create.swap(false, Ordering::SeqCst) {
                return Err(Error::Github);
            }
            Ok(Registration {
                agent_id: id,
                jit: secrecy::SecretString::from("private-test-jit"),
            })
        })
    }
    fn lookup<'a>(
        &'a self,
        name: &'a str,
    ) -> NodeFuture<'a, Option<runwell_scaleset::RunnerReference>> {
        Box::pin(async move {
            Ok(self
                .runners
                .lock()
                .await
                .iter()
                .find(|(_, n)| n.as_str() == name)
                .map(|(id, name)| runwell_scaleset::RunnerReference {
                    id: *id,
                    name: name.clone(),
                    runner_scale_set_id: 42,
                }))
        })
    }
    fn delete(&self, agent: i64) -> NodeFuture<'_, bool> {
        Box::pin(async move {
            if self.busy.load(Ordering::SeqCst) {
                return Ok(false);
            }
            self.runners.lock().await.remove(&agent);
            Ok(true)
        })
    }
}
