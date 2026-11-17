//! Original per-entry cache observations stay separate from query-hash workload aggregates.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

mod origin;
pub use origin::{CachedRuntimeCapturedPosition, CachedRuntimeReadOrigin};

pub const CACHED_RUNTIME_READ_SOURCE: &str = "statement-runtime-cache";

/// One completed per-entry source read. The caller owns source admission and shutdown; this type creates neither.
/// Native detail variants establish counter semantics. The database label or query hash never does.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct CachedRuntimeRead {
    pub origin: CachedRuntimeReadOrigin,
    pub description: CollectionDescription,
    pub observations: Vec<OperationalObservation>,
    /// Whether this carrier reconstructed every selected original row; native scope remains in the descriptor.
    pub read_complete: bool,
}

impl CachedRuntimeRead {
    pub fn is_valid(&self) -> bool {
        Self::accepts_rows(&self.description, &self.observations, self.read_complete)
            && self.origin.matches(&self.description, &self.observations, self.read_complete)
    }

    pub fn accepts_rows<'a>(
        description: &CollectionDescription,
        observations: impl IntoIterator<Item = &'a OperationalObservation>,
        read_complete: bool,
    ) -> bool {
        let Some(snapshot) = runtime_snapshot(description) else { return false };
        if !valid_description(description, snapshot) {
            return false;
        }
        let mut counts = snapshot
            .sources
            .iter()
            .map(|source| (source.source.as_str(), (source, 0usize)))
            .collect::<BTreeMap<_, _>>();
        let mut keys = BTreeSet::new();
        let mut rows = 0usize;
        for observation in observations {
            rows += 1;
            if rows > CollectionBudget::upper_limits().max_snapshot_rows as usize {
                return false;
            }
            let Some((coverage, count)) = counts.get_mut(observation.source.as_str()) else { return false };
            if !observation.is_valid()
                || !matches!(&observation.fact, OperationalFact::MssqlCachedRuntime(_))
                || !matches!(coverage.state, crate::SourceState::Collected | crate::SourceState::Partial)
                || !keys.insert((observation.source.as_str(), observation.key.as_str()))
            {
                return false;
            }
            *count += 1;
        }
        counts.values().all(|(source, count)| {
            *count <= source.records as usize && (!read_complete || *count == source.records as usize)
        })
    }

    /// An opaque reference to this original read, not a native incarnation or an authenticated archive receipt.
    pub fn read_key(&self) -> Result<String> {
        cached_statement_read_key(&self.read_id()?, &self.description)
    }

    /// Derive the opaque owner identity from the recorded original; it grants no authentication by itself.
    pub fn read_id(&self) -> Result<String> {
        self.origin.read_id()
    }

    /// A native database incarnation may survive a rename or endpoint alias. Labels and routes alone never
    /// authorize attaching an independent captured population to a scan; body/provenance validation is separate.
    pub fn matches_native_database(&self, snapshot: &crate::Snapshot) -> bool {
        let (Some(original), Some(current)) = (self.description.target.database(), snapshot.identity.as_ref()) else {
            return false;
        };
        if !snapshot.identity_names_this_database()
            || original.engine != current.engine
            || current.database.trim().is_empty()
            || !original.incarnation.as_deref().is_some_and(|value| !value.trim().is_empty())
            || !current.incarnation.as_deref().is_some_and(|value| !value.trim().is_empty())
        {
            return false;
        }
        match (original.telemetry_id(), current.telemetry_id()) {
            (Ok(original), Ok(current)) => original == current,
            _ => false,
        }
    }

    /// Structural matching of an original captured row. The input owner must authenticate the manifest separately.
    pub fn captured_references(&self) -> Result<Option<Vec<DiagnosticEvidenceRef>>> {
        if !Self::accepts_rows(&self.description, &self.observations, self.read_complete) {
            return Err(invalid());
        }
        self.origin.references(&self.description, &self.observations, self.read_complete)
    }
}

fn runtime_snapshot(description: &CollectionDescription) -> Option<&StateSnapshotDescription> {
    match &description.observation {
        SourceObservation::Snapshot(snapshot) if snapshot.dataset == SnapshotDataset::CachedRuntime => Some(snapshot),
        _ => None,
    }
}

fn valid_description(description: &CollectionDescription, snapshot: &StateSnapshotDescription) -> bool {
    description.target.database().is_some_and(|database| database.engine == crate::Engine::Mssql)
        && valid_key(&description.source)
        && description.partition_is_consistent()
        && snapshot.is_valid()
        && snapshot.sources.iter().map(|source| u64::from(source.records)).sum::<u64>()
            <= u64::from(CollectionBudget::upper_limits().max_snapshot_rows)
}

/// This source-owned read key covers runtime or separately acquired definition observations. It grants no archive authority.
pub fn cached_statement_read_key(read_id: &str, description: &CollectionDescription) -> Result<String> {
    let snapshot = description.observation.snapshot().ok_or_else(invalid)?;
    if !valid_digest(read_id)
        || !matches!(
            snapshot.dataset,
            SnapshotDataset::CachedRuntime | SnapshotDataset::CachedDefinitions | SnapshotDataset::RetainedPlans
        )
        || !description.target.is_valid()
        || !description.partition_is_consistent()
        || !snapshot.is_valid()
    {
        return Err(invalid());
    }
    let from = canonical_time(&snapshot.observed_from)?;
    let until = canonical_time(&snapshot.observed_until)?;
    serde_json::to_vec(&(
        "native-cached-statement-read-v1",
        read_id,
        &description.target,
        &description.partition,
        &description.source,
        &description.continuity,
        snapshot.dataset,
        from,
        until,
        &snapshot.sources,
    ))
    .map(|bytes| digest(&bytes))
    .map_err(|_| invalid())
}

/// The completed source owner supplies its actual identity. Hashing it neither authenticates a capture nor
/// establishes native continuity. Unknown members remain isolated to this independently owned read.
pub fn cached_statement_read_id(capture: &CaptureIdentity) -> Result<String> {
    if !valid_key(&capture.observation_id)
        || capture.revision.as_ref().is_some_and(|revision| {
            ![&revision.outbox_id, &revision.owner_epoch, &revision.sequence].into_iter().all(|field| valid_key(field))
        })
    {
        return Err(invalid());
    }
    serde_json::to_vec(&("cached-statement-read-owner-v1", capture)).map(|bytes| digest(&bytes)).map_err(|_| invalid())
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
