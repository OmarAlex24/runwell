//! Attribute observed jobs to configured hosts by runner name.
use crate::{Config, Diagnostics, Error, observation::Observation};
use std::collections::BTreeSet;

/// Without runner patterns every job stays on host 0, as in the aggregate model.
/// Otherwise a positive-work local job belongs to the one host whose patterns
/// match its runner. No match, or several, is reported instead of guessed.
pub(crate) fn attribute(
    observations: &mut [Observation<'_>],
    config: &Config,
    diagnostics: &mut Diagnostics,
) -> Result<(), Error> {
    if !config.attributes_hosts() {
        return Ok(());
    }
    let patterns = config.runner_patterns()?;
    let mut unmatched = BTreeSet::new();
    for o in observations.iter_mut() {
        if !o.local || o.net <= 0.0 {
            o.host = None;
            continue;
        }
        let runner = o.raw.runner_name.as_deref().unwrap_or_default();
        let mut hosts = patterns
            .iter()
            .enumerate()
            .filter(|(_, host)| host.iter().any(|p| p.is_match(runner)))
            .map(|(h, _)| h);
        o.host = match (hosts.next(), hosts.next()) {
            (Some(h), None) => Some(h),
            _ => None,
        };
        if o.host.is_none() {
            diagnostics.unattributed_jobs += 1;
            if !config.anonymize && !runner.is_empty() {
                unmatched.insert(runner.to_owned());
            }
        }
    }
    diagnostics.unattributed_runners = unmatched.into_iter().collect();
    if diagnostics.unattributed_jobs > 0 {
        diagnostics.warnings.push("Some jobs ran on runners that match no configured host, or several; they keep their observed duration and add no load to any host's fit.".into());
    }
    Ok(())
}
