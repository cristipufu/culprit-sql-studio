//! Current-state export projections over shared native readers. Admission and framing belong to StateSource.
use crate::{
    operations::native,
    read::{NativeRead, ReadError},
};
use culprit_core::{Result, Source, telemetry::*};

#[derive(Clone, Copy)]
pub(super) enum Family {
    Operation(native::Kind),
    FileIo,
    PerformanceCounters,
    Queue(QueueKind),
    VersionStore,
    CleanupHorizon,
    PersistentVersions,
    ProcessorTopology,
    CompilationGateways,
    MemoryClerks,
    Schedulers,
    PlanCacheSummary,
    MissingIndexes,
    Tuning(crate::query_store::tuning::Kind),
    WaitCounters,
    Replication,
    Settings(crate::settings::Kind),
}
pub(super) enum Availability {
    Available,
    MissingMetadata,
    NotApplicable,
}
pub(super) enum ReadOutcome {
    Observed(Records),
    NotApplicable,
}
pub(super) struct Records {
    pub values: Vec<TelemetryRecord>,
    pub complete_scope: bool,
    pub error_code: Option<culprit_core::ErrorCode>,
}
impl Records {
    fn complete(values: Vec<TelemetryRecord>) -> Self {
        Self { values, complete_scope: true, error_code: None }
    }
}
pub(super) fn families(dataset: SnapshotDataset) -> Result<Vec<Family>> {
    Ok(match dataset {
        SnapshotDataset::Operational => native::Kind::ALL
            .into_iter()
            .filter(|kind| *kind != native::Kind::Temp)
            .map(Family::Operation)
            .chain([Family::FileIo])
            .chain(crate::queues::KINDS.into_iter().map(Family::Queue))
            .chain([Family::VersionStore, Family::CleanupHorizon, Family::PersistentVersions])
            .collect(),
        SnapshotDataset::PlanCacheSummary => vec![Family::PlanCacheSummary],
        SnapshotDataset::MissingIndexes => vec![Family::MissingIndexes],
        SnapshotDataset::Tuning => crate::query_store::tuning::Kind::ALL.into_iter().map(Family::Tuning).collect(),
        SnapshotDataset::WaitCounters => vec![Family::WaitCounters],
        SnapshotDataset::Replication => vec![Family::Replication],
        SnapshotDataset::Settings => crate::settings::Kind::ALL.into_iter().map(Family::Settings).collect(),
        SnapshotDataset::StatementCounters
        | SnapshotDataset::StatementDefinitions
        | SnapshotDataset::Sequences
        | SnapshotDataset::Cleanup
        | SnapshotDataset::StatementGrants
        | SnapshotDataset::IndexOperations
        | SnapshotDataset::Tables
        | SnapshotDataset::ForeignKeys
        | SnapshotDataset::Columns
        | SnapshotDataset::Statistics
        | SnapshotDataset::Indexes
        | SnapshotDataset::CheckConstraints
        | SnapshotDataset::QueryCompilation
        | SnapshotDataset::CachedRuntime
        | SnapshotDataset::Objects
        | SnapshotDataset::Triggers
        | SnapshotDataset::Partitions
        | SnapshotDataset::CachedDefinitions
        | SnapshotDataset::RetainedPlans
        | SnapshotDataset::CurrentRequestMemory => {
            return Err(crate::read::invalid_data());
        }
    })
}

pub(super) fn shared_families(dataset: SnapshotDataset) -> Result<Vec<Family>> {
    if dataset != SnapshotDataset::Operational {
        return Err(crate::read::invalid_data());
    }
    Ok(vec![
        Family::Operation(native::Kind::Temp),
        Family::MemoryClerks,
        Family::Schedulers,
        Family::PerformanceCounters,
        Family::ProcessorTopology,
        Family::CompilationGateways,
    ])
}
impl Family {
    pub fn source(self) -> &'static Source {
        use crate::sources;
        match self {
            Self::Operation(kind) => kind.source(),
            Self::FileIo => &sources::FILE_LATENCY,
            Self::PerformanceCounters => &sources::PERFORMANCE_COUNTERS,
            Self::Queue(kind) => crate::queues::source(kind),
            Self::VersionStore => &sources::VERSION_STORE,
            Self::CleanupHorizon => &sources::OPEN_TRANSACTIONS,
            Self::PersistentVersions => &sources::PERSISTENT_VERSION_STORE,
            Self::ProcessorTopology => &sources::PROCESSOR_TOPOLOGY,
            Self::CompilationGateways => &sources::COMPILATION_GATEWAYS,
            Self::MemoryClerks => &sources::MEMORY_CLERKS,
            Self::Schedulers => &sources::SCHEDULERS,
            Self::PlanCacheSummary => &sources::PLAN_CACHE_USE,
            Self::MissingIndexes => &sources::MISSING_INDEXES,
            Self::Tuning(kind) => kind.source(),
            Self::WaitCounters => &sources::WAITS,
            Self::Replication => &sources::REPLICATION,
            Self::Settings(kind) => kind.source(),
        }
    }
    pub fn requires_metadata(self) -> bool {
        matches!(self, Self::Operation(native::Kind::Sequences) | Self::MissingIndexes)
    }
    pub fn availability(
        self,
        metadata_visible: bool,
        tuning_layout: Option<&Result<crate::query_store::tuning::Layout>>,
    ) -> Result<Availability> {
        if self.requires_metadata() && !metadata_visible {
            return Ok(Availability::MissingMetadata);
        }
        if let Self::Tuning(kind) = self {
            let layout = tuning_layout.ok_or_else(crate::read::invalid_data)?.as_ref().map_err(Clone::clone)?;
            return Ok(if layout.available(kind)? { Availability::Available } else { Availability::NotApplicable });
        }
        Ok(match self {
            // This capture profile is Azure SQL primary-only; it has no msdb suspect-page source.
            Self::Operation(native::Kind::SuspectPages) | Self::Settings(crate::settings::Kind::Server) => {
                Availability::NotApplicable
            }
            _ => Availability::Available,
        })
    }
    pub async fn read<R: NativeRead>(
        self,
        reader: &mut R,
        limit: usize,
        tuning_layout: Option<&Result<crate::query_store::tuning::Layout>>,
    ) -> Result<ReadOutcome> {
        let records = match self {
            Self::MemoryClerks => counter_sets(self.source().name, crate::memory_clerks::read(reader, limit).await?)?,
            Self::Schedulers => counter_sets(self.source().name, crate::schedulers::read(reader, limit).await?)?,
            Self::Tuning(kind) => {
                let layout = tuning_layout.ok_or_else(crate::read::invalid_data)?.as_ref().map_err(Clone::clone)?;
                let Some(facts) = crate::query_store::tuning::read_kind(reader, kind, layout, limit).await? else {
                    return Ok(ReadOutcome::NotApplicable);
                };
                let complete_scope=!facts.iter().any(|fact|matches!(fact,TuningFact::QueryHint(value) if value.scope==culprit_core::QueryStoreHintScope::Unknown));
                let values = facts
                    .into_iter()
                    .map(|fact| TelemetryRecord::Tuning(TuningObservation { source: kind.source().name.into(), fact }))
                    .collect();
                return Ok(ReadOutcome::Observed(Records { values, complete_scope, error_code: None }));
            }
            Self::MissingIndexes => {
                let read = crate::missing_indexes::read(reader, limit).await?;
                let error_code = read.query_link_failure.as_ref().map(|error| error.code);
                let values = read
                    .indexes
                    .into_iter()
                    .map(|native| {
                        let index = native.sample;
                        Ok(TelemetryRecord::Operational(OperationalObservation {
                            source: self.source().name.into(),
                            key: index.key()?,
                            fact: OperationalFact::MssqlMissingIndex(Box::new(index)),
                        }))
                    })
                    .collect::<Result<Vec<_>>>()?;
                return Ok(ReadOutcome::Observed(Records { values, complete_scope: error_code.is_none(), error_code }));
            }
            Self::PlanCacheSummary => {
                if limit < 6 {
                    return Err(CollectionStop::RowLimit.error());
                }
                crate::cache_summary::read(reader)
                    .await?
                    .observations()
                    .into_iter()
                    .map(TelemetryRecord::Operational)
                    .collect()
            }
            Self::ProcessorTopology => {
                if limit < 3 {
                    return Err(CollectionStop::RowLimit.error());
                }
                crate::processors::read(reader)
                    .await?
                    .observations()
                    .into_iter()
                    .map(TelemetryRecord::Operational)
                    .collect()
            }
            Self::CompilationGateways => crate::compiler_gateways::read(reader, limit)
                .await?
                .iter()
                .map(|sample| sample.observation().map(TelemetryRecord::Operational))
                .collect::<Result<Vec<_>>>()?,
            Self::PersistentVersions => {
                let Some(sample) = crate::version_store::persistent::read(reader).await? else {
                    return Ok(ReadOutcome::NotApplicable);
                };
                vec![TelemetryRecord::Operational(crate::version_store::persistent::observation(sample))]
            }
            Self::Settings(kind) => {
                crate::settings::read(reader, kind, limit).await?.into_iter().map(TelemetryRecord::Setting).collect()
            }
            Self::Operation(kind) => native::read(reader, kind, limit)
                .await?
                .observations(kind)
                .into_iter()
                .map(TelemetryRecord::Operational)
                .collect(),
            Self::FileIo => {
                let facts = crate::storage_io::read(reader, crate::storage_io::Selection::Database, limit).await?;
                crate::storage_io::observations(facts).into_iter().map(TelemetryRecord::Operational).collect()
            }
            Self::PerformanceCounters => {
                let facts = crate::performance::read(reader, limit).await?;
                crate::performance::observations(facts)?.into_iter().map(TelemetryRecord::Operational).collect()
            }
            Self::Queue(kind) => {
                let sample = crate::queues::read(reader, kind).await?;
                return Ok(ReadOutcome::Observed(Records {
                    complete_scope: sample.attribution_complete(),
                    error_code: None,
                    values: vec![TelemetryRecord::Operational(crate::queues::observation(sample))],
                }));
            }
            Self::CleanupHorizon => {
                let horizon =
                    crate::open_transactions::read(reader, crate::open_transactions::Selection::AllOtherUserSessions)
                        .await?;
                return Ok(ReadOutcome::Observed(Records {
                    complete_scope: horizon.oldest_unattributed_versioning.is_none(),
                    error_code: None,
                    values: vec![TelemetryRecord::Operational(crate::open_transactions::observation(horizon))],
                }));
            }
            Self::VersionStore => vec![TelemetryRecord::Operational(crate::version_store::observation(
                crate::version_store::read(reader).await?,
            ))],
            Self::WaitCounters => crate::waits::read(reader, true, limit)
                .await
                .map_err(ReadError::into_error)?
                .into_iter()
                .map(TelemetryRecord::WaitCounter)
                .collect(),
            Self::Replication => crate::replication::read(reader, crate::replication::Selection::GeoLinks, limit)
                .await?
                .into_iter()
                .map(|fact| {
                    TelemetryRecord::Replication(ReplicationObservation { source: self.source().name.into(), fact })
                })
                .collect(),
        };
        Ok(ReadOutcome::Observed(Records::complete(records)))
    }
}

fn counter_sets(source: &str, counters: Vec<NativeCounterSet>) -> Result<Vec<TelemetryRecord>> {
    counters
        .into_iter()
        .map(|counters| OperationalObservation::from_counters(source, counters).map(TelemetryRecord::Operational))
        .collect()
}
