//! One recorded distribution owns its identity, observation clock and population, independently of runtime totals.
use super::*;
use crate::{MeasurementScope, ResetMarker, RuntimeDistribution};

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StatementRuntimeDistribution {
    /// A provider-owned statement identifier. Qualifiers are opaque to common analysis.
    pub identity: NativeIdentity,
    pub scope: MeasurementScope,
    pub observed_at: String,
    /// No continuity with another distribution or cumulative statement point is implied.
    pub reset: ResetMarker,
    pub distribution: RuntimeDistribution,
}

impl StatementRuntimeDistribution {
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        serde_json::to_vec(&(
            &self.identity,
            self.scope,
            canonical_time(&self.observed_at)?,
            &self.distribution.source,
            self.distribution.origin,
        ))
        .map(|bytes| digest(&bytes))
        .map_err(|_| CollectionStop::InvalidPage.error())
    }

    pub fn is_valid(&self) -> bool {
        self.identity.is_valid()
            && self.observed_at.len() <= 64
            && DateTime::parse_from_rfc3339(&self.observed_at).is_ok()
            && valid_reset(&self.reset)
            && self.distribution.is_valid()
    }
}

#[cfg(test)]
mod tests;
