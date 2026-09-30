//! Linux-only Unix listener and Docker upgrade forwarding boundary.

use crate::{Error, ProxySpec};

/// Serve a per-job Docker proxy; currently unimplemented.
pub async fn serve(_spec: &ProxySpec) -> Result<(), Error> {
    Err(Error::Unimplemented)
}
