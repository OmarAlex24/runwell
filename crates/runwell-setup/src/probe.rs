//! One JSON document per host; truncated documents fail without inventing facts.
use crate::{Error, facts::HostFacts, model::HostTarget, ssh::SshClient};

pub const SCRIPT: &str = include_str!("probe.sh");

pub fn parse_output(bytes: &[u8]) -> Result<HostFacts, Error> {
    // Require an object: serde defaults must not turn null or unrelated JSON into
    // a successful host discovery. Banner text and truncated JSON are errors.
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(Error::ProbeJson)?;
    if !value.is_object() {
        return Err(Error::Usage("probe output must be a JSON object".into()));
    }
    serde_json::from_value(value).map_err(Error::ProbeJson)
}

pub async fn collect(client: &SshClient, target: &HostTarget) -> Result<HostFacts, Error> {
    client.test_auth(target).await?;
    let mut facts = parse_output(&client.probe(target).await?)?;
    crate::releases::enrich(&mut facts).await;
    Ok(facts)
}
