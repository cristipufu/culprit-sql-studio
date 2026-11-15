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
            SourceObservation::Snapshot(frame) => frame.sources.iter().all(partial),
            SourceObservation::PagedSnapshot(_) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
