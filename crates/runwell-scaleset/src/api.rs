//! Actions service endpoints. Each operation selects its own retry policy.
use crate::{
    Error,
    auth::TokenManager,
    config::Config,
    retry::Policy,
    session::Session,
    transport::{Request, Response, Transport, join, query},
    types::*,
};
use reqwest::{Method, StatusCode, Url};
use serde_json::Value;
use std::sync::Arc;

pub(crate) const SCALE_SETS: &str = "_apis/runtime/runnerscalesets";
const RUNNERS: &str = "_apis/distributedtask/pools/0/agents";

/// Cloneable client sharing cached credentials and single-flight refreshes.
#[derive(Clone)]
pub struct ActionsClient {
    pub(crate) inner: Arc<Inner>,
}
pub(crate) struct Inner {
    pub transport: Arc<Transport>,
    pub tokens: TokenManager,
    pub conflict_timeout: std::time::Duration,
}
impl ActionsClient {
    /// Construct the client without making a network request.
    pub fn new(config: Config) -> Result<Self, Error> {
        let transport = Arc::new(Transport::new(&config)?);
        let tokens = TokenManager::new(&config, transport.clone());
        Ok(Self {
            inner: Arc::new(Inner {
                transport,
                tokens,
                conflict_timeout: config.session_conflict_timeout,
            }),
        })
    }

    pub(crate) async fn admin_request(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        body: Option<Value>,
        policy: Policy,
    ) -> Result<Response, Error> {
        let mut admin = self.inner.tokens.admin(None).await?;
        for attempt in 0..=1 {
            let mut url = join(&admin.url, path);
            for (key, value) in params {
                query(&mut url, key, value);
            }
            let mut request = Request::new(method.clone(), url, policy);
            request.body = body.clone();
            let response = self
                .inner
                .transport
                .send(&request, "Bearer", &admin.token)
                .await?;
            if response.status != StatusCode::UNAUTHORIZED || attempt == 1 {
                return Ok(response);
            }
            admin = self.inner.tokens.admin(Some(&admin)).await?;
        }
        Err(Error::protocol(path, "authentication retry exhausted"))
    }
    pub(crate) async fn service_url(&self, path: &str) -> Result<Url, Error> {
        Ok(join(&self.inner.tokens.admin(None).await?.url, path))
    }

    /// Find a unique scale set within a runner group.
    pub async fn get_scale_set_by_name(
        &self,
        group: i64,
        name: &str,
    ) -> Result<Option<ScaleSet>, Error> {
        self.admin_request(
            Method::GET,
            SCALE_SETS,
            &[("runnerGroupId", group.to_string()), ("name", name.into())],
            None,
            Policy::Idempotent,
        )
        .await?
        .require(&[StatusCode::OK])?
        .json::<List<ScaleSet>>()?
        .single(SCALE_SETS)
    }
    /// List the scale sets in a runner group.
    pub async fn list_scale_sets(&self, group: i64) -> Result<Vec<ScaleSet>, Error> {
        self.admin_request(
            Method::GET,
            SCALE_SETS,
            &[("runnerGroupId", group.to_string())],
            None,
            Policy::Idempotent,
        )
        .await?
        .require(&[StatusCode::OK])?
        .json::<List<ScaleSet>>()?
        .values(SCALE_SETS)
    }
    /// Get a scale set; an external deletion remains a typed HTTP 404.
    pub async fn get_scale_set(&self, id: i64) -> Result<ScaleSet, Error> {
        self.admin_request(
            Method::GET,
            &format!("{SCALE_SETS}/{id}"),
            &[],
            None,
            Policy::Idempotent,
        )
        .await?
        .require(&[StatusCode::OK])?
        .json()
    }
    /// Create a set (HTTP 200), supplying missing System label types. Never retries
    /// transport errors or 5xx because creation is non-idempotent.
    pub async fn create_scale_set(&self, mut set: ScaleSet) -> Result<ScaleSet, Error> {
        set.normalize(true)?;
        self.admin_request(
            Method::POST,
            SCALE_SETS,
            &[],
            Some(encode(set)?),
            Policy::Never,
        )
        .await?
        .require(&[StatusCode::OK])?
        .json()
    }
    /// Apply an idempotent partial update using Go's omitted-zero field semantics.
    pub async fn update_scale_set(&self, id: i64, mut patch: ScaleSet) -> Result<ScaleSet, Error> {
        patch.normalize(false)?;
        self.admin_request(
            Method::PATCH,
            &format!("{SCALE_SETS}/{id}"),
            &[],
            Some(encode(patch)?),
            Policy::Idempotent,
        )
        .await?
        .require(&[StatusCode::OK])?
        .json()
    }
    /// Delete a set. An already deleted set is success.
    pub async fn delete_scale_set(&self, id: i64) -> Result<(), Error> {
        self.admin_request(
            Method::DELETE,
            &format!("{SCALE_SETS}/{id}"),
            &[],
            None,
            Policy::Idempotent,
        )
        .await?
        .require(&[StatusCode::NO_CONTENT, StatusCode::NOT_FOUND])?;
        Ok(())
    }
    /// Resolve exactly one runner group by name.
    pub async fn get_runner_group_by_name(&self, name: &str) -> Result<RunnerGroup, Error> {
        let path = "_apis/runtime/runnergroups/";
        self.admin_request(
            Method::GET,
            path,
            &[("groupName", name.into())],
            None,
            Policy::Idempotent,
        )
        .await?
        .require(&[StatusCode::OK])?
        .json::<List<RunnerGroup>>()?
        .single(path)?
        .ok_or_else(|| Error::protocol(path, "runner group not found"))
    }
    /// Open the single owned session, retrying conflicts within the configured window.
    pub async fn open_session(&self, id: i64, owner: impl Into<String>) -> Result<Session, Error> {
        Session::open(self.clone(), id, owner.into()).await
    }
    /// Look up a runner by name, including ownership needed for safe JIT recovery.
    pub async fn get_runner_by_name(&self, name: &str) -> Result<Option<RunnerReference>, Error> {
        self.admin_request(
            Method::GET,
            RUNNERS,
            &[("agentName", name.into())],
            None,
            Policy::Idempotent,
        )
        .await?
        .require(&[StatusCode::OK])?
        .json::<List<RunnerReference>>()?
        .single(RUNNERS)
    }
    /// Look up a runner by ID; 404 is returned as a typed HTTP error.
    pub async fn get_runner(&self, id: i64) -> Result<RunnerReference, Error> {
        self.admin_request(
            Method::GET,
            &format!("{RUNNERS}/{id}"),
            &[],
            None,
            Policy::Idempotent,
        )
        .await?
        .require(&[StatusCode::OK])?
        .json()
    }
    /// Delete registration before killing a process. A busy runner is a normal
    /// `KeepRunning` result, while unrelated conflicts remain errors.
    pub async fn remove_runner(&self, id: i64) -> Result<RemoveRunnerResult, Error> {
        let response = self
            .admin_request(
                Method::DELETE,
                &format!("{RUNNERS}/{id}"),
                &[],
                None,
                Policy::Idempotent,
            )
            .await?;
        if matches!(
            response.status,
            StatusCode::NO_CONTENT | StatusCode::NOT_FOUND
        ) {
            return Ok(RemoveRunnerResult::SafeToKill);
        }
        let error = response.error();
        if error.status() == Some(StatusCode::CONFLICT) && error.is_type("JobStillRunningException")
        {
            return Ok(RemoveRunnerResult::KeepRunning);
        }
        Err(error)
    }
    /// Register a runner once. On `AgentExistsException`, remove the colliding runner
    /// only if it belongs to this set and is not busy, then regenerate exactly once.
    /// No automatic retry follows an ambiguous transport error or 5xx.
    pub async fn generate_jit_config(
        &self,
        id: i64,
        settings: &JitSettings,
    ) -> Result<JitConfig, Error> {
        let path = format!("{SCALE_SETS}/{id}/generatejitconfig");
        let body = encode(settings)?;
        let response = self
            .admin_request(Method::POST, &path, &[], Some(body.clone()), Policy::Never)
            .await?;
        if response.status == StatusCode::OK {
            return response.json();
        }
        let error = response.error();
        if error.status() != Some(StatusCode::CONFLICT) || !error.is_type("AgentExistsException") {
            return Err(error);
        }
        if let Some(runner) = self.get_runner_by_name(&settings.name).await?
            && (runner.runner_scale_set_id != id
                || self.remove_runner(runner.id).await? == RemoveRunnerResult::KeepRunning)
        {
            return Err(error);
        }
        self.admin_request(Method::POST, &path, &[], Some(body), Policy::Never)
            .await?
            .require(&[StatusCode::OK])?
            .json()
    }
}
fn encode(value: impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|_| Error::Config("request cannot be serialized"))
}
