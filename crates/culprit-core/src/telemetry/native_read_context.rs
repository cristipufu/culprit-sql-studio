//! Read ownership does not establish the execution origin or lifetime represented by retained native counters.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeReadContext {
    pub target: CollectionTarget,
    pub partition: SourcePartition,
    /// Bounds of the retained native observations. They are not counter lifetimes or acquisition-cost measurements.
    pub observed_from: String,
    pub observed_until: String,
    /// Native updateability observed on this connection. It grants no permission to execute a change.
    pub database_writable: Option<bool>,
}

impl NativeReadContext {
    /// Native Activity header bounds and source ownership only. The caller authenticates the original capture
    /// and its rows independently; this does not grant native admission or a request generation.
    pub fn from_activity(source: &CollectionDescription) -> Result<Self> {
        let native = source.observation.activity().ok_or_else(|| CollectionStop::InvalidPage.error())?;
        if !source.partition_is_consistent() || !native.is_valid() || native.coverage.source != source.source {
            return Err(CollectionStop::InvalidPage.error());
        }
        let context = Self {
            target: source.target.clone(),
            partition: source.partition.clone(),
            observed_from: native.observed_from.clone(),
            observed_until: native.observed_until.clone(),
            database_writable: native.database_writable,
        };
        if !context.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(context)
    }

    /// Exact known owner/updateability and ordered native observation bounds. Matching endpoints do not prove that the
    /// process, role or participant remained continuously unchanged between those observations.
    pub fn matches_earlier_scope(&self, original: &Self) -> bool {
        self.same_qualified_owner(original)
            && ObservationBounds::new(&original.observed_until, &self.observed_from).is_some()
    }

    /// Confirm one already-acquired read owner while preserving its original start. This is neither data
    /// namespace qualification nor permission to run a source question on that member.
    pub fn completes_read(&self, original: &Self) -> bool {
        self.same_qualified_owner(original)
            && same_time(&self.observed_from, &original.observed_from)
            && ObservationBounds::new(&original.observed_until, &self.observed_until).is_some()
    }

    fn same_qualified_owner(&self, original: &Self) -> bool {
        self.has_qualified_local_owner()
            && original.has_qualified_local_owner()
            && original.database_writable.is_some()
            && self.target == original.target
            && self.partition == original.partition
            && self.database_writable == original.database_writable
    }

    pub fn from_snapshot(source: &CollectionDescription) -> Result<Self> {
        let snapshot = source.observation.snapshot().ok_or_else(|| CollectionStop::InvalidPage.error())?;
        if !source.partition_is_consistent() || !snapshot.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        let context = Self {
            target: source.target.clone(),
            partition: source.partition.clone(),
            observed_from: snapshot.observed_from.clone(),
            observed_until: snapshot.observed_until.clone(),
            database_writable: match &source.observation {
                SourceObservation::SuspectPages { database_writable, .. }
                | SourceObservation::DatabasePerformanceCounters { database_writable, .. }
                | SourceObservation::BackupHistory { database_writable, .. }
                | SourceObservation::ColumnstoreRowgroups { database_writable, .. }
                | SourceObservation::TableMemory { database_writable, .. } => Some(*database_writable),
                SourceObservation::Jobs { metadata, .. } => metadata.database_writable,
                SourceObservation::MysqlEvents { metadata, .. } => Some(metadata.database_writable),
                SourceObservation::MaintenanceProgress { result, .. }
                | SourceObservation::CurrentRequestWork { result, .. } => result.database_writable,
                SourceObservation::PageWaitTargets { database_writable, .. }
                | SourceObservation::PageWaitWriters { database_writable, .. }
                | SourceObservation::PageTaskHolders { database_writable, .. } => *database_writable,
                _ => None,
            },
        };
        if !context.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(context)
    }

    pub fn is_valid(&self) -> bool {
        self.target.is_valid()
            && self.partition.is_valid_for(&self.target)
            && self.observed_from.len() <= 64
            && self.observed_until.len() <= 64
            && ObservationBounds::new(&self.observed_from, &self.observed_until).is_some()
    }

    pub fn has_qualified_local_owner(&self) -> bool {
        self.is_valid()
            && matches!(self.partition, SourcePartition::InstanceLocal { .. })
            && self.partition.is_qualified()
    }

    /// Cache identity for one observed native request; it grants neither admission nor incident attribution.
    pub fn request_key(&self, actor: &ActivityKey, statement: &NativeIdentity) -> Option<String> {
        if !self.has_qualified_local_owner() || !actor.is_valid() || actor.request.is_none() || !statement.is_valid() {
            return None;
        }
        serde_json::to_vec(&(
            self.target.telemetry_id().ok()?,
            &self.partition,
            self.database_writable,
            actor,
            statement,
        ))
        .ok()
        .map(|bytes| digest(&bytes))
    }
}

#[cfg(test)]
pub(super) mod tests;
