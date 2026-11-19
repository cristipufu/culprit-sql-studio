//! Cumulative statement totals are point observations, never fabricated native intervals.
use super::*;
#[cfg(test)]
mod tests;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StatementCounter {
    pub key: RuntimeMetricKey,
    pub unit: RuntimeMetricUnit,
    pub total: MeasurementValue,
}
impl StatementCounter {
    fn is_valid(&self) -> bool {
        self.key.accepts(self.unit)
            && self.total.is_valid()
            && (!matches!(
                self.key,
                RuntimeMetricKey::Executions
                    | RuntimeMetricKey::Errors
                    | RuntimeMetricKey::Warnings
                    | RuntimeMetricKey::Rows
                    | RuntimeMetricKey::TemporaryBlocksRead
                    | RuntimeMetricKey::TemporaryBlocksWritten
                    | RuntimeMetricKey::TemporaryTables
                    | RuntimeMetricKey::TemporaryDiskTables
                    | RuntimeMetricKey::SortMergePasses
            ) || matches!(self.total, MeasurementValue::Integer(_)))
    }
}
/// Some sources reset extrema independently from their cumulative mean and variance.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct StatementDistribution {
    pub statistics: RuntimeMetric,
    pub reset: crate::ResetMarker,
    pub extrema_reset: crate::ResetMarker,
}
impl StatementDistribution {
    fn is_valid(&self) -> bool {
        self.statistics.is_valid() && valid_reset(&self.reset) && valid_reset(&self.extrema_reset)
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StatementCounterSample {
    /// Source-owned point membership. Native producers establish this before publication; labels never do.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub member: Option<SharedResourceIdentity>,
    pub identity: NativeQueryIdentity,
    /// A database observed on one distributed node is not a database-wide or cluster-wide total.
    pub scope: crate::MeasurementScope,
    pub observed_at: String,
    /// Unknown remains unknown: two observations alone do not establish a safe delta.
    pub reset: crate::ResetMarker,
    pub totals: Vec<StatementCounter>,
    pub distributions: Vec<StatementDistribution>,
}
impl StatementCounterSample {
    /// Storage admission binds typed membership and the native point clock to the original source header.
    pub fn fits_source_scope(&self, source: &CollectionDescription) -> bool {
        if !self.is_valid() || source.target.database().is_none() {
            return false;
        }
        if let SourceObservation::PagedSnapshot(header) | SourceObservation::PagedSourceRead { snapshot: header, .. } =
            &source.observation
        {
            // Staging has only its native lower bound. The same source owner's final header must admit the
            // original retained row again before the capture can become complete.
            return header.dataset == SnapshotDataset::StatementCounters
                && header.is_valid()
                && source.partition_is_consistent()
                && ObservationBounds::new(&header.observed_from, &self.observed_at).is_some()
                && self.matches_partition(&source.partition, source.partition.is_qualified());
        }
        NativeReadContext::from_snapshot(source).is_ok_and(|context| {
            source.observation.snapshot().is_some_and(|header| header.dataset == SnapshotDataset::StatementCounters)
                && ObservationBounds::new(&context.observed_from, &context.observed_until)
                    .is_some_and(|bounds| bounds.contains(&self.observed_at))
                && self.matches_population(&context)
        })
    }

    fn matches_population(&self, context: &NativeReadContext) -> bool {
        self.matches_partition(&context.partition, context.has_qualified_local_owner())
    }

    fn matches_partition(&self, partition: &SourcePartition, owner_qualified: bool) -> bool {
        match self.scope {
            crate::MeasurementScope::Database => true,
            crate::MeasurementScope::Node => matches!(partition,
                SourcePartition::InstanceLocal { member: Some(member), epoch: Some(_) }
                    if owner_qualified && self.member.as_ref() == Some(member)),
            _ => false,
        }
    }

    /// Only source-declared semantic keys are interpreted. Native names and engine identity grant no meaning.
    pub fn outcomes(&self) -> crate::Result<crate::StatementOutcomeCounts> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        let total = |key: &RuntimeMetricKey| -> crate::Result<Option<String>> {
            match self.totals.iter().find(|counter| &counter.key == key).map(|counter| &counter.total) {
                None => Ok(None),
                Some(MeasurementValue::Integer(value)) => Ok(Some(value.clone())),
                Some(MeasurementValue::Real(_)) => Err(CollectionStop::InvalidPage.error()),
            }
        };
        Ok(crate::StatementOutcomeCounts {
            errors: total(&RuntimeMetricKey::Errors)?,
            warnings: total(&RuntimeMetricKey::Warnings)?,
        })
    }
    pub fn key(&self) -> crate::Result<String> {
        serde_json::to_vec(&self.identity).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        let mut keys = std::collections::BTreeSet::new();
        let mut distributions = std::collections::BTreeSet::new();
        self.identity.is_valid()
            && match (self.scope, self.member.as_ref()) {
                (crate::MeasurementScope::Database, None) => true,
                (crate::MeasurementScope::Node, Some(member)) => {
                    member.is_valid() && member.kind == SharedResourceKind::Node
                }
                _ => false,
            }
            && valid_reset(&self.reset)
            && self.observed_at.len() <= 64
            && DateTime::parse_from_rfc3339(&self.observed_at).is_ok()
            && !self.totals.is_empty()
            && self.totals.len() <= MAX_STATEMENT_METRICS
            && self.totals.iter().all(|metric| metric.is_valid() && keys.insert(&metric.key))
            && self.distributions.len() <= MAX_STATEMENT_METRICS
            && self
                .distributions
                .iter()
                .all(|distribution| distribution.is_valid() && distributions.insert(&distribution.statistics.key))
    }
}

/// One source's original counter roster. A missing native clock/owner is never replaced by a metadata probe epoch.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct StatementCounterSnapshot {
    pub source: String,
    pub context: Option<Box<NativeReadContext>>,
    pub coverage: StateSourceCoverage,
    pub observations: Vec<OperationalObservation>,
}

impl StatementCounterSnapshot {
    pub fn unavailable(source: &str, error_code: crate::ErrorCode, stopped: Option<CollectionStop>) -> Self {
        Self {
            source: source.into(),
            context: None,
            coverage: StateSourceCoverage::unavailable(source.into(), error_code, stopped),
            observations: vec![],
        }
    }
    pub fn from_source(
        source: &CollectionDescription,
        observations: Vec<OperationalObservation>,
    ) -> crate::Result<Self> {
        let frame = source.observation.snapshot().ok_or_else(|| CollectionStop::InvalidPage.error())?;
        if frame.dataset != SnapshotDataset::StatementCounters
            || frame.sources.len() != 1
            || !source.partition_is_consistent()
        {
            return Err(CollectionStop::InvalidPage.error());
        }
        let snapshot = Self {
            source: frame.sources[0].source.clone(),
            context: Some(Box::new(NativeReadContext::from_snapshot(source)?)),
            coverage: frame.sources[0].clone(),
            observations,
        };
        snapshot.is_valid_read(false).then_some(snapshot).ok_or_else(|| CollectionStop::InvalidPage.error())
    }

    pub fn is_valid(&self) -> bool {
        self.is_valid_read(true)
    }

    pub fn is_valid_read(&self, read_complete: bool) -> bool {
        let limit = CollectionBudget::upper_limits().max_snapshot_rows as usize;
        if !valid_key(&self.source)
            || !self.coverage.is_valid()
            || self.coverage.source != self.source
            || self.coverage.records as usize > limit
            || self.observations.len() > self.coverage.records as usize
            || read_complete && self.observations.len() != self.coverage.records as usize
        {
            return false;
        }
        let Some(context) = self.context.as_deref() else {
            return self.observations.is_empty()
                && !self.coverage.complete_scope
                && matches!(
                    self.coverage.state,
                    crate::SourceState::Partial | crate::SourceState::Unavailable | crate::SourceState::NotApplicable
                );
        };
        if !context.is_valid()
            || context.target.database().is_none()
            || self.coverage.complete_scope && !context.partition.is_qualified()
        {
            return false;
        }
        let Some(bounds) = ObservationBounds::new(&context.observed_from, &context.observed_until) else {
            return false;
        };
        let mut keys = std::collections::BTreeSet::new();
        self.observations.iter().all(|row| {
            if !row.is_valid()
                || row.source != self.source
                || !keys.insert(&row.key)
                || !matches!(self.coverage.state, crate::SourceState::Collected | crate::SourceState::Partial)
            {
                return false;
            }
            let OperationalFact::StatementCounters(sample) = &row.fact else { return false };
            bounds.contains(&sample.observed_at) && sample.matches_population(context)
        })
    }
}
