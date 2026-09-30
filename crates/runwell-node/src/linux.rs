//! Linux-only systemd, cgroup reading, and PSI trigger boundary.

use crate::{Error, SliceSpec};

/// Create a limited slice before a runner or Docker scope can reference it.
/// MemorySwapMax must be zero and IOAccounting enabled. Currently unimplemented.
pub fn create_slice(_spec: &SliceSpec) -> Result<(), Error> {
    Err(Error::Unimplemented)
}
