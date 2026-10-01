use crate::Error;
use crate::workspace_trust::WorkspaceTrust;
use runwell_config::{GithubConfig, StandaloneConfig};
use runwell_store::{Job, State};
use runwell_workspace::{Cache, CacheKey, Owner};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub(super) struct Workspaces {
    cache: Arc<Mutex<Cache>>,
    run: PathBuf,
    scope: Option<String>,
    repositories: BTreeMap<String, String>,
    per_class: bool,
    trust: Option<WorkspaceTrust>,
}
impl Workspaces {
    pub fn new(
        settings: &StandaloneConfig,
        owner: Owner,
        github: Option<&GithubConfig>,
    ) -> Result<Self, Error> {
        settings
            .validate_runtime_paths()
            .map_err(|_| Error::Config)?;
        let parent = settings.runners_dir.parent().ok_or(Error::Config)?;
        let mut config = settings.workspace.clone();
        if config.cache_root.is_none() {
            config.cache_root = Some(parent.join("caches"));
        }
        let run = parent.join("workspaces");
        for root in [&run, config.cache_root.as_ref().ok_or(Error::Config)?] {
            for other in [&settings.runners_dir, &settings.templates_dir] {
                if root.starts_with(other) || other.starts_with(root) {
                    return Err(Error::Config);
                }
            }
        }
        let scope = github
            .and_then(|g| reqwest::Url::parse(&g.config_url).ok())
            .and_then(|u| {
                let repo = u.path().trim_matches('/');
                (!repo.starts_with("enterprises/") && runwell_config::valid_repository(repo))
                    .then(|| repo.to_owned())
            });
        Ok(Self {
            cache: Arc::new(Mutex::new(Cache::open(config, &run, owner)?)),
            run,
            scope,
            repositories: settings.workspace.repositories.clone(),
            per_class: settings.workspace.per_class,
            trust: github.map(WorkspaceTrust::new).transpose()?,
        })
    }
    async fn blocking<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Cache) -> Result<T, runwell_workspace::Error> + Send + 'static,
    ) -> Result<T, Error> {
        let cache = self.cache.clone();
        tokio::task::spawn_blocking(move || {
            let mut cache = cache
                .lock()
                .map_err(|_| runwell_workspace::Error::Invalid)?;
            operation(&mut cache)
        })
        .await
        .map_err(|_| Error::Io)?
        .map_err(Error::from)
    }
    pub async fn prepare(&self, job: &Job) -> Result<(), Error> {
        // Scale sets cannot force a request->runner binding. Only a repository
        // scope (or administrator-declared per-class routing) can safely seed.
        let repo = self
            .scope
            .as_ref()
            .or_else(|| self.repositories.get(&job.metadata.class));
        let key = repo
            .map(|repo| CacheKey::new(repo, self.per_class.then_some(job.metadata.class.as_str())))
            .transpose()?;
        if key.is_none() {
            tracing::warn!(
                job_id = job.id,
                "workspace is cold: configure repository routing for organization scale sets"
            );
        }
        let id = job.id as u64;
        self.blocking(move |cache| cache.prepare(id, key).map(|_| ()))
            .await
    }
    pub fn home(&self, id: u64) -> Result<PathBuf, Error> {
        Ok(self.run.join("jobs").join(id.to_string()).join("home"))
    }
    pub async fn bind(
        &self,
        id: u64,
        execution: runwell_workspace::Execution,
    ) -> Result<(), Error> {
        match self.blocking(move |cache| cache.bind(id, execution)).await {
            Err(Error::Workspace(runwell_workspace::Error::Io(e)))
                if e.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(())
            }
            result => result,
        }
    }
    pub async fn reconcile(&self, retained: HashSet<u64>) -> Result<(), Error> {
        self.blocking(move |cache| cache.reconcile(&retained)).await
    }
    pub async fn harvest(&self, job: &Job) {
        // Cache failures never turn successful workflow execution into failure.
        if job.state != State::Completed || job.actual_request_id.is_none() {
            return;
        }
        let id = job.id as u64;
        let evidence = self.blocking(move |cache| cache.execution(id)).await;
        if !matches!(evidence, Ok(Some(ref e)) if Some(e.request_id) == job.actual_request_id
            && e.workflow_run_id == job.metadata.workflow_run_id && e.repository.eq_ignore_ascii_case(&job.metadata.repo))
        {
            tracing::warn!(
                job_id = id,
                "workspace promotion skipped: no complete actual assignment evidence"
            );
            return;
        }
        let Some(trust) = &self.trust else {
            return;
        };
        let completion = match trust.completion(job).await {
            Ok(completion) => completion,
            Err(error) => {
                tracing::warn!(%error, job_id = job.id, "workspace promotion skipped: cannot verify ref");
                return;
            }
        };
        let id = job.id as u64;
        let now = jiff::Timestamp::now().as_second().max(0) as u64;
        if let Err(error) = self
            .blocking(move |cache| cache.promote(id, &completion, now))
            .await
        {
            tracing::warn!(%error, job_id = id, "workspace promotion skipped");
        }
    }
    pub async fn teardown(&self, id: u64) -> Result<(), Error> {
        match self
            .blocking(move |cache| {
                cache.teardown(id)?;
                cache.gc()
            })
            .await
        {
            Err(Error::Workspace(runwell_workspace::Error::Detached)) => Ok(()),
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn home_and_tokio_heartbeat_do_not_wait_for_cache_copy_lock() {
        let temp = tempfile::tempdir().unwrap();
        let run = temp.path().join("run");
        let config = runwell_config::WorkspaceConfig {
            cache_root: Some(temp.path().join("cache")),
            ..Default::default()
        };
        let owner = Owner {
            uid: rustix::process::geteuid().as_raw(),
            gid: rustix::process::getegid().as_raw(),
        };
        let workspaces = Arc::new(Workspaces {
            cache: Arc::new(Mutex::new(Cache::copy(config, &run, owner).unwrap())),
            run: run.clone(),
            scope: None,
            repositories: BTreeMap::new(),
            per_class: false,
            trust: None,
        });
        let (ready, started) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let worker = workspaces.clone();
        let copying = tokio::spawn(async move {
            worker
                .blocking(move |_| {
                    ready.send(()).unwrap();
                    // A deadline also lets the regression fail instead of hanging.
                    let _ = blocked.recv_timeout(std::time::Duration::from_secs(3));
                    Ok(())
                })
                .await
                .unwrap();
        });
        started.await.unwrap();
        let before = std::time::Instant::now();
        assert_eq!(workspaces.home(2).unwrap(), run.join("jobs/2/home"));
        tokio::spawn(async {}).await.unwrap();
        let elapsed = before.elapsed();
        let _ = release.send(());
        copying.await.unwrap();
        assert!(elapsed < std::time::Duration::from_secs(1));
    }
}
