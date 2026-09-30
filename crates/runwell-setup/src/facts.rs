//! Typed observations. Missing values always carry a reason, including old probes.
use serde::{Deserialize, Deserializer, Serialize};

/// An observation or an explicit explanation of why it is unknown.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Fact<T> {
    pub value: Option<T>,
    pub unknown_reason: Option<String>,
}

impl<T> Default for Fact<T> {
    fn default() -> Self {
        Self::unknown("not reported by probe")
    }
}

impl<T> Fact<T> {
    pub fn known(value: T) -> Self {
        Self {
            value: Some(value),
            unknown_reason: None,
        }
    }

    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            value: None,
            unknown_reason: Some(reason.into()),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Fact<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire<T> {
            value: Option<T>,
            unknown_reason: Option<String>,
        }
        let wire = Option::<Wire<T>>::deserialize(deserializer)?;
        Ok(match wire {
            Some(Wire {
                value: Some(value), ..
            }) => Self::known(value),
            Some(Wire {
                unknown_reason: Some(reason),
                ..
            }) if !reason.trim().is_empty() => Self::unknown(reason),
            _ => Self::default(),
        })
    }
}

/// Linux host observations; numeric sizes use bytes and CPU/load use numbers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostFacts {
    pub os: Fact<String>,
    pub kernel: Fact<String>,
    pub arch: Fact<String>,
    pub vcpus: Fact<u32>,
    pub cpu_model: Fact<String>,
    pub hypervisor: Fact<String>,
    pub kvm: Fact<bool>,
    pub ram_bytes: Fact<u64>,
    pub swap_total_bytes: Fact<u64>,
    pub swap_used_bytes: Fact<u64>,
    pub swappiness: Fact<u32>,
    pub root_fs_type: Fact<String>,
    pub root_size_bytes: Fact<u64>,
    pub root_free_bytes: Fact<u64>,
    pub disk_scheduler: Fact<String>,
    pub cgroup_v2: Fact<bool>,
    pub controllers: Fact<Vec<String>>,
    pub enabled_controllers: Fact<Vec<String>>,
    pub psi: Fact<bool>,
    pub pressure: Fact<Pressure>,
    pub systemd_version: Fact<String>,
    pub docker_present: Fact<bool>,
    pub docker_version: Fact<String>,
    pub docker_cgroup_driver: Fact<String>,
    pub docker_root_dir: Fact<String>,
    pub docker_root_usage_bytes: Fact<u64>,
    pub runners: Fact<Vec<RunnerFacts>>,
    pub load_average: Fact<[f64; 3]>,
    pub sar_installed: Fact<bool>,
    pub daily_cpu: Fact<Vec<DailyCpu>>,
    pub listening_services: Fact<Vec<ListeningService>>,
    pub sudo_available: Fact<bool>,
}

/// PSI percentages over the last 10, 60 and 300 seconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Pressure {
    pub cpu_some: Fact<[f64; 3]>,
    pub cpu_full: Fact<[f64; 3]>,
    pub memory_some: Fact<[f64; 3]>,
    pub memory_full: Fact<[f64; 3]>,
    pub io_some: Fact<[f64; 3]>,
    pub io_full: Fact<[f64; 3]>,
}

/// One discovered systemd Actions runner, without credentials.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunnerFacts {
    pub unit: Fact<String>,
    pub scope: Fact<String>,
    pub name: Fact<String>,
    pub labels: Fact<Vec<String>>,
    pub version: Fact<String>,
    pub release_age_days: Fact<u64>,
    pub ephemeral: Fact<bool>,
    pub user: Fact<String>,
    pub home: Fact<String>,
    pub active_job: Fact<bool>,
}

/// Per-day utilization percentiles computed from sar's aggregate CPU samples.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DailyCpu {
    pub date: Fact<String>,
    pub p50: Fact<f64>,
    pub p95: Fact<f64>,
    pub samples: Fact<u64>,
}

/// Listening process details, potentially incomplete for an unprivileged user.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ListeningService {
    pub name: Fact<String>,
    pub endpoint: Fact<String>,
}
