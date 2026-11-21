//! A source declares whether facts belong to its target or one connected native compute member. Routing intent,
//! replica role and observation clocks never supply member identity or a native process/counter generation.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum SourcePartition {
    /// The source-owned target namespace. This does not establish a physical compute member.
    Target,
    InstanceLocal {
        member: Option<SharedResourceIdentity>,
        /// A source-owned native generation, separate from replica role and collector ownership epochs.
        epoch: Option<String>,
    },
}

impl SourcePartition {
    pub fn is_valid_for(&self, target: &CollectionTarget) -> bool {
        match self {
            Self::Target => target.is_valid(),
            Self::InstanceLocal { member, epoch } => {
                target.is_valid()
                    && member.as_ref().is_none_or(|member| {
                        member.is_valid()
                            && member.engine == target.engine()
                            && matches!(member.kind, SharedResourceKind::Instance | SharedResourceKind::Node)
                    })
                    && epoch.as_deref().is_none_or(valid_key)
            }
        }
    }

    /// Only a declared target namespace or a known member and native generation can establish continuity.
    pub fn is_qualified(&self) -> bool {
        match self {
            Self::Target => true,
            Self::InstanceLocal { member, epoch } => member.is_some() && epoch.is_some(),
        }
    }

    /// A native restart remains the same member; two unidentified members never establish equality.
    pub fn same_member(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Target, Self::Target) => true,
            (Self::InstanceLocal { member: Some(left), .. }, Self::InstanceLocal { member: Some(right), .. }) => {
                left.telemetry_id().ok().zip(right.telemetry_id().ok()).is_some_and(|(left, right)| left == right)
            }
            _ => false,
        }
    }

    pub fn native_epoch(&self) -> Option<&str> {
        match self {
            Self::Target => None,
            Self::InstanceLocal { epoch, .. } => epoch.as_deref(),
        }
    }
}

impl CollectionDescription {
    pub fn is_partition_qualified(&self) -> bool {
        self.partition.is_valid_for(&self.target) && self.partition.is_qualified()
    }

    /// An unknown local member preserves partial native points. It cannot publish complete current heads,
    /// authorize absence, or provide a stable native definition/history namespace.
    pub fn partition_is_consistent(&self) -> bool {
        if !self.partition.is_valid_for(&self.target) {
            return false;
        }
        if matches!(self.observation, SourceObservation::MssqlPoolCurrent { .. }) {
            return mssql_pool_current_authority(self).is_ok_and(|authority| authority.is_some());
        }
        if self.observation.collection() == TelemetryCollection::Snapshot(SnapshotDataset::WaitCounters)
            && matches!(&self.target, CollectionTarget::Shared(pool) if pool.engine == crate::Engine::Mssql && pool.kind == SharedResourceKind::Pool)
        {
            return false;
        }
        if self.observation.collection() == TelemetryCollection::Snapshot(SnapshotDataset::ProcessCpu) {
            return cpu_profile_header_is_valid(self);
        }
        if self.observation.collection() == TelemetryCollection::Snapshot(SnapshotDataset::NumaNodes) {
            return numa_node_header_is_valid(self);
        }
        if let SourceObservation::MssqlManagedInstanceResources(history) = &self.observation {
            return history.matches_source(self);
        }
        if self.observation.collection() == TelemetryCollection::Snapshot(SnapshotDataset::SuspectPages) {
            return suspect_page_context(self).is_ok();
        }
        if self.observation.collection() == TelemetryCollection::Snapshot(SnapshotDataset::DatabasePerformanceCounters)
        {
            return database_performance_context(self).is_ok();
        }
        if self.observation.collection() == TelemetryCollection::Snapshot(SnapshotDataset::BackupHistory)
            && !matches!(self.observation, SourceObservation::BackupHistory { .. })
        {
            return false;
        }
        if matches!(self.observation, SourceObservation::BackupHistory { .. }) {
            return backup_history_context(self).is_ok();
        }
        if matches!(&self.observation, SourceObservation::Snapshot(snapshot) if snapshot.dataset == SnapshotDataset::PageWaitTargets)
        {
            return false;
        }
        if let Some(execution) = self.observation.execution_context() {
            let Ok(scope) = NativeSourceReadScope::from_context(execution) else {
                return false;
            };
            let header_valid = match &self.observation {
                SourceObservation::SourceRead { .. } => self.observation.covers_request(),
                SourceObservation::PagedSourceRead { snapshot, focus_key, .. } => {
                    snapshot.is_valid()
                        && SourceReadQuestion::for_dataset(snapshot.dataset).is_some()
                        && valid_digest(focus_key)
                        && same_time(&snapshot.observed_from, &execution.observed_from)
                }
                _ => false,
            };
            return header_valid && self.partition == SourcePartition::Target && scope.matches_target(&self.target);
        }
        if let SourceObservation::QueryHistory(history) = &self.observation
            && history.acquisition_owner.as_ref().is_some_and(|owner| {
                self.partition != SourcePartition::Target || owner.target != self.target || !history.is_valid()
            })
        {
            return false;
        }
        if let SourceObservation::Jobs { snapshot, metadata } = &self.observation {
            return metadata.matches_source(self, snapshot);
        }
        if let SourceObservation::MysqlEvents { snapshot, metadata } = &self.observation {
            return metadata.matches_source(self, snapshot);
        }
        if let SourceObservation::MssqlAgentJobs { snapshot, metadata } = &self.observation {
            return metadata.matches_source(self, snapshot);
        }
        if let SourceObservation::FocusedBlocking { snapshot, result } = &self.observation {
            return result.is_valid_source(self, snapshot)
                && matches!(&self.partition, SourcePartition::InstanceLocal { epoch, .. } if epoch == &snapshot.epoch);
        }
        if let SourceObservation::MaintenanceProgress { snapshot, result }
        | SourceObservation::CurrentRequestWork { snapshot, result, .. } = &self.observation
        {
            return result.is_valid_source(self, snapshot);
        }
        if let SourceObservation::PageWaitTargets { database_writable, original_activity, .. } = &self.observation
            && (!self.observation.covers_request()
                || self.target.database().is_none() && (database_writable.is_some() || original_activity.is_some()))
        {
            return false;
        }
        if let SourceObservation::PageTaskHolders { snapshot, focus, .. } = &self.observation {
            return self.observation.covers_request()
                && self.target == focus.participant.target
                && self.partition == focus.participant.partition
                && matches!(
                    (&focus.page.native_key, snapshot.sources.as_slice()),
                    (NativeRecordIdentity::Operational { source, .. }, [coverage]) if coverage.source == *source
                )
                && matches!(self.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) });
        }
        if let SourceObservation::PageWaitWriters { focus, .. } = &self.observation {
            let original = &focus.waiter.participant;
            return self.observation.covers_request()
                && self.target == original.target
                && self.partition == original.partition
                && matches!(self.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) });
        }
        if self.observation.collection() == TelemetryCollection::Snapshot(SnapshotDataset::GroupReplication)
            && (!matches!(self.observation, SourceObservation::GroupReplication { .. })
                || group_replication_context(self).is_err())
        {
            return false;
        }
        if matches!(self.observation, SourceObservation::MssqlPoolSettings { .. })
            && mssql_pool_settings_membership(self).is_err()
        {
            return false;
        }
        if matches!(self.observation, SourceObservation::MssqlReplication { .. })
            && mssql_replication_scope(self).is_err()
        {
            return false;
        }
        if self.observation.collection() == TelemetryCollection::Snapshot(SnapshotDataset::Galera)
            && (!matches!(self.observation, SourceObservation::Galera { .. }) || galera_context(self).is_err())
        {
            return false;
        }
        if matches!(self.observation, SourceObservation::TableMemory { .. }) && table_memory_context(self).is_err() {
            return false;
        }
        if matches!(self.observation, SourceObservation::ColumnstoreRowgroups { .. })
            && columnstore_rowgroup_context(self).is_err()
        {
            return false;
        }
        if matches!(self.observation, SourceObservation::FullTextPopulations { .. })
            && full_text_population_context(self).is_err()
        {
            return false;
        }
        if let SourceObservation::CacheEntryMemory { snapshot, focus } = &self.observation
            && (!focus.is_valid()
                || focus.context.target != self.target
                || focus.context.partition != self.partition
                || !snapshot.is_valid()
                || snapshot.dataset != SnapshotDataset::CacheEntryMemory
                || ObservationBounds::new(&focus.context.observed_until, &snapshot.observed_from).is_none())
        {
            return false;
        }
        if let SourceObservation::Diagnostics(description) = &self.observation
            && (!description.is_valid()
                || description
                    .scope_bindings
                    .values()
                    .any(|scope| scope.target.telemetry_id().ok() != self.target.telemetry_id().ok()))
        {
            return false;
        }
        if let (SourcePartition::InstanceLocal { epoch, .. }, SourceObservation::Activity(frame)) =
            (&self.partition, &self.observation)
            && epoch != &frame.epoch
        {
            return false;
        }
        if self.is_partition_qualified() {
            return true;
        }
        let partial = |coverage: &StateSourceCoverage| {
            !coverage.complete_scope
                && matches!(
                    coverage.state,
                    crate::SourceState::Partial | crate::SourceState::Unavailable | crate::SourceState::NotApplicable
                )
        };
        match &self.observation {
            SourceObservation::Activity(frame) => partial(&frame.coverage),
            SourceObservation::Snapshot(frame) | SourceObservation::PageWaitTargets { snapshot: frame, .. } => {
                frame.sources.iter().all(partial)
            }
            SourceObservation::PagedSnapshot(_) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
