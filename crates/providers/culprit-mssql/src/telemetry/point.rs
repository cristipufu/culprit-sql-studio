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
    DatabasePerformanceCounters,
    Queue(QueueKind),
    VersionStore,
    CleanupHorizon,
    PersistentVersions,
    ProcessorTopology,
    CompilationGateways,
    MemoryClerks,
    TempdbMetadataMemory,
    InstanceResources,
    Schedulers,
    MemoryFailures,
    PlanCacheSummary,
    MissingIndexes,
    Tuning(crate::query_store::tuning::Kind),
    WaitCounters,
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
        SnapshotDataset::Operational => native::Kind::SCAN
            .into_iter()
            .filter(|kind| *kind != native::Kind::Temp)
            .map(Family::Operation)
            .chain([Family::FileIo])
            .chain(crate::queues::KINDS.into_iter().map(Family::Queue))
            .chain([Family::VersionStore, Family::CleanupHorizon, Family::PersistentVersions])
            .collect(),
        SnapshotDataset::DatabasePerformanceCounters => vec![Family::DatabasePerformanceCounters],
        SnapshotDataset::PlanCacheSummary => vec![Family::PlanCacheSummary],
        SnapshotDataset::MissingIndexes => vec![Family::MissingIndexes],
        SnapshotDataset::Tuning => crate::query_store::tuning::Kind::ALL.into_iter().map(Family::Tuning).collect(),
        SnapshotDataset::WaitCounters => vec![Family::WaitCounters],
        SnapshotDataset::Settings => crate::settings::Kind::ALL.into_iter().map(Family::Settings).collect(),
        SnapshotDataset::HyperscaleLogComponents
        | SnapshotDataset::StatementCounters
        | SnapshotDataset::RuntimeDistributions
        | SnapshotDataset::StatementDefinitions
        | SnapshotDataset::Sequences
        | SnapshotDataset::Cleanup
        | SnapshotDataset::StatementGrants
        | SnapshotDataset::CacheEntryMemory
        | SnapshotDataset::IndexOperations
        | SnapshotDataset::Tables
        | SnapshotDataset::ForeignKeys
        | SnapshotDataset::Columns
        | SnapshotDataset::Statistics
        | SnapshotDataset::Indexes
        | SnapshotDataset::CheckConstraints
        | SnapshotDataset::QueryCompilation
        | SnapshotDataset::CompilerAdmission
        | SnapshotDataset::MaintenanceProgress
        | SnapshotDataset::CurrentRequestWork
        | SnapshotDataset::CachedRuntime
        | SnapshotDataset::Objects
        | SnapshotDataset::Triggers
        | SnapshotDataset::Partitions
        | SnapshotDataset::CachedDefinitions
        | SnapshotDataset::RetainedPlans
        | SnapshotDataset::CurrentRequestMemory => {
            return Err(crate::read::invalid_data());
        }
        SnapshotDataset::Replication
        | SnapshotDataset::MemoryFailures
        | SnapshotDataset::NativeFailures
        | SnapshotDataset::Jobs
        | SnapshotDataset::ColumnstoreRowgroups
        | SnapshotDataset::Cdc
        | SnapshotDataset::BackupHistory
        | SnapshotDataset::SessionMemory
        | SnapshotDataset::PageWaitTargets
        | SnapshotDataset::TaskSpace
        | SnapshotDataset::PageWaitWriters
        | SnapshotDataset::PageTaskHolders => return Err(crate::read::invalid_data()),
    })
}

pub(super) fn shared_families(dataset: SnapshotDataset) -> Result<Vec<Family>> {
    if dataset == SnapshotDataset::MemoryFailures {
        return Ok(vec![Family::MemoryFailures]);
    }
    if dataset != SnapshotDataset::Operational {
        return Err(crate::read::invalid_data());
    }
    Ok(vec![
        Family::Operation(native::Kind::Temp),
        Family::Operation(native::Kind::TempLog),
        Family::MemoryClerks,
        Family::TempdbMetadataMemory,
        Family::InstanceResources,
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
            Self::PerformanceCounters | Self::DatabasePerformanceCounters => &sources::PERFORMANCE_COUNTERS,
            Self::Queue(kind) => crate::queues::source(kind),
            Self::VersionStore => &sources::VERSION_STORE,
            Self::CleanupHorizon => &sources::OPEN_TRANSACTIONS,
            Self::PersistentVersions => &sources::PERSISTENT_VERSION_STORE,
            Self::ProcessorTopology => &sources::PROCESSOR_TOPOLOGY,
            Self::CompilationGateways => &sources::COMPILATION_GATEWAYS,
            Self::MemoryClerks => &sources::MEMORY_CLERKS,
            Self::TempdbMetadataMemory => &sources::TEMPDB_METADATA_MEMORY,
            Self::InstanceResources => &sources::INSTANCE_RESOURCES,
            Self::Schedulers => &sources::SCHEDULERS,
            Self::MemoryFailures => &sources::MEMORY_FAILURES,
            Self::PlanCacheSummary => &sources::PLAN_CACHE_USE,
            Self::MissingIndexes => &sources::MISSING_INDEXES,
            Self::Tuning(kind) => kind.source(),
            Self::WaitCounters => &sources::WAITS,
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
        owner: Option<&super::subject::Identity>,
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
            Self::Operation(native::Kind::SuspectPages) => Availability::NotApplicable,
            Self::Settings(crate::settings::Kind::Server)
                if !matches!(owner, Some(super::subject::Identity::LocalDatabase(_))) =>
            {
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
        owner: Option<&super::subject::Identity>,
    ) -> Result<ReadOutcome> {
        let records = match self {
            Self::MemoryFailures => {
                let Some(facts) = crate::oom_events::read(reader, limit).await? else {
                    return Ok(ReadOutcome::NotApplicable);
                };
                facts
                    .into_iter()
                    .map(|fact| {
                        Ok(TelemetryRecord::Operational(OperationalObservation {
                            source: self.source().name.into(),
                            key: fact.key()?,
                            fact: OperationalFact::MssqlOutOfMemory(Box::new(fact)),
                        }))
                    })
                    .collect::<Result<Vec<_>>>()?
            }
            Self::InstanceResources => {
                if limit == 0 {
                    return Err(CollectionStop::RowLimit.error());
                }
                let point = crate::resources::read_instance_pressure(reader)
                    .await
                    .map_err(ReadError::into_error)?
                    .observation()?;
                let complete_scope = point.has_complete_population();
                let row = OperationalObservation {
                    source: self.source().name.into(),
                    key: point.key()?,
                    fact: OperationalFact::MssqlInstanceResources(Box::new(point)),
                };
                return Ok(ReadOutcome::Observed(Records {
                    values: vec![TelemetryRecord::Operational(row)],
                    complete_scope,
                    error_code: None,
                }));
            }
            Self::MemoryClerks => counter_sets(self.source().name, crate::memory_clerks::read(reader, limit).await?)?,
            Self::TempdbMetadataMemory => {
                let Some(super::subject::Identity::Instance(owner)) = owner else {
                    return Ok(ReadOutcome::NotApplicable);
                };
                let Some(read) = crate::tempdb_metadata_memory::read(reader, owner, limit).await? else {
                    return Ok(ReadOutcome::NotApplicable);
                };
                return Ok(ReadOutcome::Observed(Records {
                    values: counter_sets(self.source().name, read.counters)?,
                    complete_scope: read.complete_scope,
                    error_code: read.error_code,
                }));
            }
            Self::Schedulers => counter_sets(self.source().name, crate::schedulers::read(reader, limit).await?)?,
            Self::Tuning(kind) => {
                let layout = tuning_layout.ok_or_else(crate::read::invalid_data)?.as_ref().map_err(Clone::clone)?;
                let Some(facts) = crate::query_store::tuning::read_kind(reader, kind, layout, limit).await? else {
                    return Ok(ReadOutcome::NotApplicable);
                };
                let complete_scope = kind.has_complete_attribution(&layout.plan_scope(), &facts);
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
                let values = match owner {
                    Some(super::subject::Identity::LocalDatabase(context)) => {
                        crate::settings::read_current(reader, kind, limit, context).await?
                    }
                    _ => crate::settings::read(reader, kind, limit).await?,
                };
                values.into_iter().map(TelemetryRecord::Setting).collect()
            }
            Self::Operation(kind) => native::read(reader, kind, limit)
                .await?
                .observations(kind)?
                .into_iter()
                .map(TelemetryRecord::Operational)
                .collect(),
            Self::FileIo => {
                let facts = crate::storage_io::read(reader, crate::storage_io::Selection::Database, limit).await?;
                crate::storage_io::observations(facts).into_iter().map(TelemetryRecord::Operational).collect()
            }
            Self::DatabasePerformanceCounters => {
                let Some(super::subject::Identity::LocalDatabase(owner)) = owner else {
                    return Err(crate::read::invalid_data());
                };
                let profile =
                    culprit_core::Engine::Mssql.spec().performance_counters().ok_or_else(crate::read::invalid_data)?;
                let facts = crate::performance::read_current(reader, profile.database_field_ids.len(), owner).await?;
                let complete_scope = facts.native_rows == profile.database_field_ids.len();
                let values =
                    crate::performance::observations(facts)?.into_iter().map(TelemetryRecord::Operational).collect();
                return Ok(ReadOutcome::Observed(Records { values, complete_scope, error_code: None }));
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

#[cfg(test)]
mod tests;
