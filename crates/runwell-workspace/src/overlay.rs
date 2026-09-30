//! Linux-only host-namespace overlay mount boundary.

use crate::{Error, WorkspaceSpec};

/// Mount with redirect_dir=on, without modifying any lower; currently unimplemented.
pub fn prepare(_spec: &WorkspaceSpec) -> Result<(), Error> {
    Err(Error::Unimplemented)
}
