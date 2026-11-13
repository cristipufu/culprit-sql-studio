//! Tracked session allocations are neither execution grants nor process resident memory. A current event is
//! independently observed session context; it does not identify the statement that made an allocation.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_SESSION_MEMORY_OBSERVATIONS: usize = 4096;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct SessionMemoryContext {
    pub request: NativeActivityIdentity,
    pub observed_at: String,
    pub database: Option<String>,
    pub query_hash: Option<String>,
}

impl SessionMemoryContext {
    pub fn is_valid(&self) -> bool {
        self.request.is_valid()
            && self.observed_at.len() <= 64
            && canonical_time(&self.observed_at).is_ok()
            && self.database.as_deref().is_none_or(identifier)
            && self.query_hash.as_deref().is_none_or(identifier)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct SessionMemoryObservation {
    pub source: String,
    pub identity: NativeIdentity,
    pub observed_at: String,
    pub session: NativeActivityIdentity,
    /// Only the native tracking state at this read; it cannot qualify the session's earlier lifetime.
    pub tracking_enabled: bool,
    pub current_bytes: Option<String>,
    pub controlled_current_bytes: Option<String>,
    pub high_water_bytes: Option<String>,
    pub controlled_high_water_bytes: Option<String>,
    pub current_context: Option<SessionMemoryContext>,
    /// Native instrumentation/configuration values retain their names without common interpretation.
    pub native_properties: BTreeMap<String, String>,
}

impl SessionMemoryObservation {
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(&self.identity).map(|value| digest(&value)).map_err(|_| CollectionStop::InvalidPage.error())
    }

    pub fn is_valid(&self) -> bool {
        let values = [
            &self.current_bytes,
            &self.controlled_current_bytes,
            &self.high_water_bytes,
            &self.controlled_high_water_bytes,
        ];
        valid_key(&self.source)
            && self.identity.is_valid()
            && self.session.is_valid()
            && self.observed_at.len() <= 64
            && canonical_time(&self.observed_at).is_ok()
            && values.into_iter().flatten().all(|value| valid_counter(value))
            && (self.tracking_enabled || values.into_iter().all(Option::is_none))
            && self.current_context.as_ref().is_none_or(SessionMemoryContext::is_valid)
            && native_properties_valid(&self.native_properties)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct SessionMemorySnapshot {
    pub source: String,
    pub observed_from: String,
    pub observed_until: String,
    pub resource: SharedResourceIdentity,
    pub observations: Vec<SessionMemoryObservation>,
}

impl SessionMemorySnapshot {
    pub fn is_valid(&self) -> bool {
        let Some(bounds) = ObservationBounds::new(&self.observed_from, &self.observed_until) else {
            return false;
        };
        let mut keys = BTreeSet::new();
        valid_key(&self.source)
            && self.resource.is_valid()
            && self.resource.kind == SharedResourceKind::Instance
            && self.observed_from.len() <= 64
            && self.observed_until.len() <= 64
            && self.observations.len() <= MAX_SESSION_MEMORY_OBSERVATIONS
            && self.observations.iter().all(|row| {
                row.is_valid()
                    && row.source == self.source
                    && bounds.contains(&row.observed_at)
                    && row.current_context.as_ref().is_none_or(|context| bounds.contains(&context.observed_at))
                    && row.key().is_ok_and(|key| keys.insert(key))
            })
    }
}

#[cfg(test)]
mod tests;
