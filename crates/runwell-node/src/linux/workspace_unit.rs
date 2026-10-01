use super::systemd::{Properties, property};
use crate::Error;
use std::path::Path;
use zbus::zvariant::Value;

/// Mask the workspace tree with a root-owned read-only tmpfs and bind only this
/// HOME back at its original path, so Docker still resolves host bind sources.
/// Unlike InaccessiblePaths, TemporaryFileSystem allows nested BindPaths.
pub(super) fn properties(home: Option<&Path>) -> Result<Properties, Error> {
    let mut properties = Vec::new();
    if let Some(home) = home {
        let root = home
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or(Error::Config)?;
        properties.push(property(
            "TemporaryFileSystem",
            Value::new(vec![(
                root.to_str().ok_or(Error::Config)?.to_owned(),
                "ro,mode=0711,nodev,nosuid".to_owned(),
            )]),
        )?);
        properties.push(property(
            "BindPaths",
            Value::new(vec![(
                home.to_str().ok_or(Error::Config)?.to_owned(),
                home.to_str().ok_or(Error::Config)?.to_owned(),
                false,
                0_u64,
            )]),
        )?);
        // Separate capability domains prevent same-UID /proc/<peer>/root and
        // ptrace access from bypassing the per-unit mount namespace.
        properties.push(property("PrivateUsers", true)?);
    }
    Ok(properties)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binds_only_own_home_into_masked_workspaces() {
        let properties = properties(Some(Path::new("/state/workspaces/jobs/2/home"))).unwrap();
        let masked: Vec<(String, String)> =
            properties[0].1.try_clone().unwrap().try_into().unwrap();
        assert_eq!(
            masked,
            vec![(
                "/state/workspaces".into(),
                "ro,mode=0711,nodev,nosuid".into()
            )]
        );
        let binds: Vec<(String, String, bool, u64)> =
            properties[1].1.try_clone().unwrap().try_into().unwrap();
        assert_eq!(
            binds,
            vec![(
                "/state/workspaces/jobs/2/home".into(),
                "/state/workspaces/jobs/2/home".into(),
                false,
                0
            )]
        );
        assert!(bool::try_from(properties[2].1.try_clone().unwrap()).unwrap());
    }
}
