//! Optional, identity-free JSONL/TOML availability input. Times are UTC instants.
use crate::{Config, Error};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Half-open offline intervals and installed pool capacity changes.
/// Pool indices refer to the order of `[[pools]]` in configuration. Runner indices
/// are stable zero-based identities within that pool on the indicated host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Record {
    /// Absolute installed capacity, effective at `at` until the next change.
    PoolSize {
        /// Configured host index.
        host: usize,
        /// Configured pool index.
        pool: usize,
        /// Change instant.
        at: Timestamp,
        /// Installed slots (zero disables dispatch).
        runners: usize,
    },
    /// A listener cannot accept new work; active jobs continue to completion.
    RunnerOffline {
        /// Configured host index.
        host: usize,
        /// Configured pool index.
        pool: usize,
        /// Stable ordinal, starting at zero.
        runner: usize,
        /// Inclusive beginning.
        start: Timestamp,
        /// Exclusive recovery instant.
        end: Timestamp,
        /// Evidence category; it does not change interval semantics.
        cause: Cause,
    },
    /// No dispatch or work progress on the host until recovery. Reservations stay
    /// held; this models suspension, not loss/restart of in-flight jobs.
    HostOffline {
        /// Configured host index.
        host: usize,
        /// Inclusive beginning.
        start: Timestamp,
        /// Exclusive recovery instant.
        end: Timestamp,
    },
}
/// Why a listener is unavailable.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// Listener service stopped or has not reached listening state.
    Service,
    /// Polling failed until an observed recovery.
    Broker,
}
/// Parse JSON Lines without performing filesystem access or leaking input values.
pub fn parse_jsonl(input: &str) -> Result<Vec<Record>, Error> {
    input
        .lines()
        .enumerate()
        .filter(|(_, s)| !s.trim().is_empty())
        .map(|(i, s)| {
            serde_json::from_str(s)
                .map_err(|_| Error::Invalid(format!("invalid availability JSON on line {}", i + 1)))
        })
        .collect()
}
/// Parse a TOML document containing `[[events]]` tables.
pub fn parse_toml(input: &str) -> Result<Vec<Record>, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Document {
        events: Vec<Record>,
    }
    toml::from_str::<Document>(input)
        .map(|d| d.events)
        .map_err(|_| Error::Invalid("invalid availability TOML".into()))
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum Action {
    Size {
        host: usize,
        pool: usize,
        runners: usize,
    },
    Runner {
        host: usize,
        pool: usize,
        runner: usize,
        offline: bool,
    },
    Host {
        host: usize,
        offline: bool,
    },
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct Change {
    pub time: f64,
    pub action: Action,
}

pub(crate) fn prepare(
    config: &Config,
    mapping: &BTreeMap<usize, usize>,
    origin: f64,
) -> Result<Vec<Change>, Error> {
    let mut changes = Vec::new();
    let mut sizes = BTreeSet::new();
    let time = |t: Timestamp| t.as_millisecond() as f64 / 1000.0 - origin;
    for record in &config.availability {
        let (host, pool) = match *record {
            Record::PoolSize { host, pool, .. } | Record::RunnerOffline { host, pool, .. } => {
                (host, Some(pool))
            }
            Record::HostOffline { host, .. } => (host, None),
        };
        if host >= config.hosts.len() {
            return Err(Error::Invalid(
                "availability host index out of range".into(),
            ));
        }
        let pool = pool
            .map(|p| {
                if p >= config.pools.len() {
                    return Err(Error::Invalid(
                        "availability pool index out of range".into(),
                    ));
                }
                mapping.get(&p).copied().ok_or_else(|| {
                    Error::Invalid("availability pool has no local executions".into())
                })
            })
            .transpose()?;
        match *record {
            Record::PoolSize {
                at,
                runners,
                pool: original,
                ..
            } => {
                let Some(pool) = pool else { continue };
                if config
                    .runner_history
                    .iter()
                    .any(|c| c.host == host && c.repo == config.pools[original].repo)
                {
                    return Err(Error::Invalid(
                        "use availability or runner_history for a pool, not both".into(),
                    ));
                }
                if runners > 100_000 || !sizes.insert((host, pool, at)) {
                    return Err(Error::Invalid(
                        "duplicate or excessive availability capacity".into(),
                    ));
                }
                changes.push(Change {
                    time: time(at),
                    action: Action::Size {
                        host,
                        pool,
                        runners,
                    },
                });
            }
            Record::RunnerOffline {
                runner,
                start,
                end,
                pool: original,
                ..
            } => {
                let Some(pool) = pool else { continue };
                let maximum = config
                    .availability
                    .iter()
                    .filter_map(|r| match *r {
                        Record::PoolSize {
                            host: h,
                            pool: p,
                            runners,
                            ..
                        } if h == host && p == original => Some(runners),
                        _ => None,
                    })
                    .fold(config.pools[original].runners, usize::max);
                if end <= start || runner >= maximum || runner >= 100_000 {
                    return Err(Error::Invalid("invalid runner offline interval".into()));
                }
                for (t, offline) in [(start, true), (end, false)] {
                    changes.push(Change {
                        time: time(t),
                        action: Action::Runner {
                            host,
                            pool,
                            runner,
                            offline,
                        },
                    });
                }
            }
            Record::HostOffline { start, end, .. } => {
                if end <= start {
                    return Err(Error::Invalid("invalid host offline interval".into()));
                }
                for (t, offline) in [(start, true), (end, false)] {
                    changes.push(Change {
                        time: time(t),
                        action: Action::Host { host, offline },
                    });
                }
            }
        }
    }
    Ok(changes)
}
