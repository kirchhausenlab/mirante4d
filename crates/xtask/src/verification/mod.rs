mod fixtures;
mod registry;
mod runner;

pub(crate) use runner::{Leaf, verify_leaf, verify_local, verify_pr};

pub(crate) fn verification_sync(check: bool) -> anyhow::Result<()> {
    registry::sync_generated(check)
}
