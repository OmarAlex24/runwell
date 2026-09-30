//! Best-effort release age enrichment on the local machine, never on the host.
use crate::facts::{Fact, HostFacts};
use serde::Deserialize;
use std::{collections::HashMap, time::Duration};

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    published_at: String,
}

fn age(json: &[u8], version: &str, now: jiff::Timestamp) -> Fact<u64> {
    let Ok(release) = serde_json::from_slice::<Release>(json) else {
        return Fact::unknown("runner release response was not valid metadata");
    };
    if release.tag_name != format!("v{version}") {
        return Fact::unknown("release tag did not match the installed runner");
    }
    let Ok(published) = release.published_at.parse::<jiff::Timestamp>() else {
        return Fact::unknown("runner publication timestamp unavailable");
    };
    let seconds = now.as_second().saturating_sub(published.as_second());
    match u64::try_from(seconds) {
        Ok(seconds) => Fact::known(seconds / 86_400),
        Err(_) => Fact::unknown("runner publication timestamp is in the future"),
    }
}

/// Public GitHub release metadata needs no credentials. Offline and rate-limited
/// results remain unknown. No hostname or repository input is sent to GitHub.
pub(crate) async fn enrich(facts: &mut HostFacts) {
    let Some(runners) = &mut facts.runners.value else {
        return;
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("runwell-setup")
        .build();
    let Ok(client) = client else {
        return;
    };
    let mut cache = HashMap::new();
    for runner in runners {
        let Some(version) = &runner.version.value else {
            continue;
        };
        if let Some(known) = cache.get(version) {
            runner.release_age_days = Fact::clone(known);
            continue;
        }
        // Bound requests and allow only conventional release versions, not paths.
        if cache.len() >= 8
            || version.split('.').count() != 3
            || !version.bytes().all(|c| c.is_ascii_digit() || c == b'.')
        {
            runner.release_age_days =
                Fact::unknown("runner release version invalid or lookup limit reached");
            continue;
        }
        let result = match client
            .get(format!(
                "https://api.github.com/repos/actions/runner/releases/tags/v{version}"
            ))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => match response.bytes().await {
                Ok(bytes) => age(&bytes, version, jiff::Timestamp::now()),
                Err(_) => Fact::unknown("runner release metadata could not be read"),
            },
            _ => Fact::unknown(
                "public runner release metadata unavailable (offline, missing, or rate-limited)",
            ),
        };
        cache.insert(version.clone(), result.clone());
        runner.release_age_days = result;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_age_uses_publication_date_and_rejects_bad_metadata() {
        let now = "2026-09-30T00:00:00Z".parse().unwrap();
        let json = br#"{"tag_name":"v2.325.0","published_at":"2026-08-01T00:00:00Z"}"#;
        assert_eq!(age(json, "2.325.0", now).value, Some(60));
        assert!(age(json, "2.326.0", now).unknown_reason.is_some());
        assert!(age(b"garbled", "2.325.0", now).unknown_reason.is_some());
    }
}
