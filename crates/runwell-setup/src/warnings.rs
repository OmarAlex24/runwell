//! Derived warnings distinguish known absences from missing observations.
use crate::{facts::HostFacts, model::RepoTrace};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Warning {
    pub code: String,
    pub message: String,
}

pub fn derive(facts: &HostFacts, traces: &[RepoTrace]) -> Vec<Warning> {
    let mut warnings = Vec::new();
    let mut add = |code: &str, message: &str| {
        warnings.push(Warning {
            code: code.into(),
            message: message.into(),
        })
    };
    if facts.cgroup_v2.value == Some(false) {
        add(
            "no_cgroup_v2",
            "cgroup v2 is unavailable; resource isolation requires it",
        );
    }
    if facts.psi.value == Some(false) {
        add(
            "no_psi",
            "PSI is unavailable; pressure-aware admission cannot use host pressure",
        );
    }
    if facts.docker_present.value == Some(false)
        && traces
            .iter()
            .any(|trace| trace.uses_service_containers == Some(true))
    {
        add(
            "no_docker",
            "Jobs use service containers but Docker is absent",
        );
    }
    if let (Some(total), Some(used)) = (facts.swap_total_bytes.value, facts.swap_used_bytes.value)
        && total > 0
        && used as f64 / total as f64 >= 0.5
    {
        add(
            "swap_heavily_used",
            "At least half of swap is in use; memory headroom may be insufficient",
        );
    }
    if let Some(services) = &facts.listening_services.value
        && services.iter().any(|service| {
            service.name.value.as_ref().is_some_and(|name| {
                [
                    "postgres",
                    "mysqld",
                    "mariadbd",
                    "redis-server",
                    "mongod",
                    "java",
                    "nginx",
                    "apache2",
                    "httpd",
                    "kubelet",
                    "containerd",
                ]
                .iter()
                .any(|heavy| name.contains(heavy))
            })
        })
    {
        add(
            "host_not_dedicated",
            "Other substantial services listen on this host; reserve capacity for them",
        );
    }
    if let Some(runners) = &facts.runners.value {
        if runners
            .iter()
            .any(|runner| runner.release_age_days.value.is_some_and(|days| days > 30))
        {
            add(
                "old_runner",
                "A runner release is older than 30 days; check its update policy",
            );
        }
        for (index, runner) in runners.iter().enumerate() {
            if runner.ephemeral.value != Some(false) {
                continue;
            }
            if runners[..index].iter().any(|other| {
                other.ephemeral.value == Some(false)
                    && (runner
                        .home
                        .value
                        .as_ref()
                        .is_some_and(|home| other.home.value.as_ref() == Some(home))
                        || runner
                            .user
                            .value
                            .as_ref()
                            .is_some_and(|user| other.user.value.as_ref() == Some(user)))
            }) {
                add(
                    "shared_home",
                    "Persistent runners share a user or HOME; job state can leak between runners",
                );
                break;
            }
        }
    }
    warnings
}
