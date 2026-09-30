//! Wire types following `actions/scaleset` e6daac7 `types.go`.
use crate::{Error, Secret};
use serde::{Deserialize, Serialize};

/// Absolute service counters; scale from assigned jobs, never the event count.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Statistics {
    /// Jobs available for acquisition.
    pub total_available_jobs: u32,
    /// Jobs acquired by this set.
    pub total_acquired_jobs: u32,
    /// Assigned jobs, including running jobs.
    pub total_assigned_jobs: u32,
    /// Currently executing jobs.
    pub total_running_jobs: u32,
    /// Registered runners.
    pub total_registered_runners: u32,
    /// Busy runners.
    pub total_busy_runners: u32,
    /// Idle runners.
    pub total_idle_runners: u32,
}

/// A workflow routing label.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Label {
    /// Label category, defaulted to `System` on create and update.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Label text used in `runs-on`.
    pub name: String,
}

/// Settings applied to runners in the set.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RunnerSetting {
    /// Whether runners must be updated by the external supervisor.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disable_update: bool,
}
fn zero(id: &i64) -> bool {
    *id == 0
}
fn zero_time() -> String {
    "0001-01-01T00:00:00Z".into()
}

/// Scale-set request/response. Zero-valued identity fields are omitted on PATCH;
/// `RunnerSetting` and `createdOn` retain Go HEAD's legacy serialization.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScaleSet {
    /// Server identity; omit on create.
    #[serde(skip_serializing_if = "zero")]
    pub id: i64,
    /// Name unique within a runner group.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Runner group identity.
    #[serde(skip_serializing_if = "zero")]
    pub runner_group_id: i64,
    /// Server-resolved group name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub runner_group_name: String,
    /// Routing labels.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<Label>,
    /// Legacy key is capitalized exactly as Go HEAD serializes it.
    #[serde(rename = "RunnerSetting")]
    pub runner_setting: RunnerSetting,
    /// Server creation time, with Go's zero time on new requests.
    pub created_on: String,
    /// Server-resolved JIT endpoint.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub runner_jit_config_url: String,
    /// Absolute demand snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statistics: Option<Statistics>,
}
impl Default for ScaleSet {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            runner_group_id: 0,
            runner_group_name: String::new(),
            labels: Vec::new(),
            runner_setting: RunnerSetting::default(),
            created_on: zero_time(),
            runner_jit_config_url: String::new(),
            statistics: None,
        }
    }
}
impl ScaleSet {
    pub(crate) fn normalize(&mut self, create: bool) -> Result<(), Error> {
        if create && self.labels.is_empty() {
            if self.name.is_empty() {
                return Err(Error::Config("a scale set needs a name or label"));
            }
            self.labels.push(Label {
                name: self.name.clone(),
                kind: "System".into(),
            });
        }
        for label in &mut self.labels {
            if label.kind.is_empty() {
                label.kind = "System".into();
            }
        }
        Ok(())
    }
}

/// Runner group metadata.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerGroup {
    /// Group identity.
    pub id: i64,
    /// Group name.
    pub name: String,
    /// Number of runners.
    #[serde(default)]
    pub size: u32,
    /// Whether this is the default group.
    #[serde(default)]
    pub is_default_group: bool,
}

/// Registered runner identity, including scale-set ownership.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerReference {
    /// Agent ID for removal.
    pub id: i64,
    /// Unique agent name.
    pub name: String,
    /// Owning scale set, checked before collision recovery deletes a runner.
    pub runner_scale_set_id: i64,
}

/// Input for non-idempotent runner registration.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JitSettings {
    /// Persist this unique name before issuing the POST.
    pub name: String,
    /// Runner work directory (empty selects the service default).
    pub work_folder: String,
}

/// JIT registration result; persist the runner ID before starting its process.
#[derive(Clone, Debug, Deserialize)]
pub struct JitConfig {
    /// Server-side runner registration.
    pub runner: RunnerReference,
    /// Secret including the runner's credentials and private key.
    #[serde(rename = "encodedJITConfig")]
    pub encoded_jit_config: Secret,
}

/// Result of the atomic service-side busy check during scale-down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveRunnerResult {
    /// Registration was removed or already absent; the process can be killed.
    SafeToKill,
    /// The service reports a running job; preserve the process and retry later.
    KeepRunning,
}

#[derive(Deserialize)]
pub(crate) struct List<T> {
    pub count: usize,
    pub value: Vec<T>,
}
impl<T> List<T> {
    pub fn values(self, endpoint: &str) -> Result<Vec<T>, Error> {
        if self.count != self.value.len() {
            return Err(Error::protocol(
                endpoint,
                "list count does not match values",
            ));
        }
        Ok(self.value)
    }
    pub fn single(self, endpoint: &str) -> Result<Option<T>, Error> {
        let mut values = self.values(endpoint)?;
        if values.len() > 1 {
            return Err(Error::protocol(
                endpoint,
                "multiple matches for unique name",
            ));
        }
        Ok(values.pop())
    }
}
