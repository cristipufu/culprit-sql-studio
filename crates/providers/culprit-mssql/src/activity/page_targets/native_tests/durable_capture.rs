//! Page scenarios adapt their admitted fixture to the shared native capture owner.
use super::fixture::{Fixture, invalid};
pub(super) use crate::test_support::owned_captures::{Captured, OwnedCaptures};
use culprit_core::Result;

impl OwnedCaptures {
    pub(super) fn new(fixture: &Fixture) -> Result<Self> {
        Self::from_pool(
            fixture.pool.as_ref().ok_or_else(|| invalid("owned fixture pool absent"))?.clone(),
            fixture.port,
        )
    }
}
