//! Bounded current facts. Each native source retains its own availability and visibility contract.
use super::{point, progress::Progress, subject};
use crate::{
    conn::Pool,
    read::{NativeRead, ReadError, ReadSession, integer, invalid_data, stops_remaining_reads, text},
};
use culprit_core::{ErrorCode, Result, telemetry::*};
use std::sync::Arc;
const CLOCK: &str = r#"SELECT /* culprit */ CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,
 HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS metadata_visibility"#;
const ENVELOPE_ALLOWANCE: usize = 4096;
const MAX_BUFFERED_FAMILY_ROWS: usize = 10_000;
const SNAPSHOT_CLOCK_QUERIES: u32 = 2;
#[cfg(test)]
mod database_counters;
mod local_point;
mod pool_current;
pub(super) use pool_current::bounded_budget as pool_current_budget;
mod numa_nodes;
mod process_cpu;
pub(super) use numa_nodes::bounded_budget as numa_nodes_budget;
pub(super) use process_cpu::bounded_budget as process_cpu_budget;
mod pool_settings;
mod settings;
pub(super) use pool_settings::bounded_budget as pool_settings_budget;
pub(super) mod suspect_pages;
mod tuning;
pub(super) use local_point::bounded_budget as local_point_budget;
pub(super) use settings::bounded_budget as settings_budget;
pub(super) use tuning::bounded_budget as tuning_budget;

fn native_budget(request: &CollectionRequest) -> CollectionBudget {
    if pool_current::selected(request) {
        pool_current_budget(&request.budget)
    } else if request.selection == (CollectionSelection::Snapshot { dataset: SnapshotDataset::ProcessCpu }) {
        process_cpu_budget(&request.budget)
    } else if request.selection == (CollectionSelection::Snapshot { dataset: SnapshotDataset::NumaNodes }) {
        numa_nodes_budget(&request.budget)
    } else if pool_settings::selected(request) {
        pool_settings_budget(&request.budget)
    } else if request.selection == (CollectionSelection::Snapshot { dataset: SnapshotDataset::MemoryFailures }) {
        memory_failure_budget(&request.budget)
    } else if request.selection == (CollectionSelection::Snapshot { dataset: SnapshotDataset::Tuning }) {
        tuning_budget(&request.budget)
    } else if request.selection
        == (CollectionSelection::Snapshot { dataset: SnapshotDataset::DatabasePerformanceCounters })
        || request.selection == (CollectionSelection::Snapshot { dataset: SnapshotDataset::SuspectPages })
    {
        local_point_budget(&request.budget)
    } else if request.selection == (CollectionSelection::Snapshot { dataset: SnapshotDataset::Settings }) {
        settings_budget(&request.budget)
    } else {
        request.budget.clone()
    }
}
pub(super) fn memory_failure_budget(requested: &CollectionBudget) -> CollectionBudget {
    ReadSession::budget_including_setup(
        requested,
        crate::memory_failures::bounded_budget(requested, SNAPSHOT_CLOCK_QUERIES),
    )
}

pub(super) async fn clock<R: NativeRead>(reader: &mut R) -> Result<(String, bool)> {
    let rows = reader.query(CLOCK, vec![], 1).await.map_err(ReadError::into_error)?;
    let row = rows.first().ok_or_else(invalid_data)?;
    Ok((canonical_time(&text(row, "observed_at")?)?, integer(row, "metadata_visibility")? == 1))
}

pub(super) struct StateSource<R = ReadSession> {
    request: CollectionRequest,
    host: String,
    port: u16,
    io: R,
    description: Option<CollectionDescription>,
    identity: Option<subject::Identity>,
    database_authority: subject::DatabaseAuthority,
    point_generation: Option<u64>,
    rows: Vec<TelemetryRecord>,
    pool_settings_point: Option<MssqlPoolSettingsPoint>,
    offset: usize,
    progress: Progress,
}
impl StateSource {
    pub(super) fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        Self::with_io(ReadSession::collection(pool, native_budget(request))?, request, host, port)
    }
}
impl<R: NativeRead> StateSource<R> {
    fn with_io(io: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        let dataset = pool_settings::dataset(&request.selection)?;
        if !pool_settings::selected(request)
            && dataset == SnapshotDataset::Settings
            && matches!(&request.subject, CollectionSubject::Shared { .. })
        {
            return Err(invalid_data());
        }
        subject::validate_dataset(&request.subject, dataset)?;
        let database_authority = subject::DatabaseAuthority::for_source(dataset, &request.subject)?;
        let mut request = request.clone();
        if pool_current::selected(&request)
            || matches!(
                dataset,
                SnapshotDataset::ProcessCpu
                    | SnapshotDataset::NumaNodes
                    | SnapshotDataset::Tuning
                    | SnapshotDataset::Settings
                    | SnapshotDataset::DatabasePerformanceCounters
                    | SnapshotDataset::SuspectPages
            )
        {
            request.budget = native_budget(&request);
            request.budget.validate()?;
        }
        Ok(Self {
            request,
            host,
            port,
            io,
            description: None,
            identity: None,
            database_authority,
            point_generation: None,
            rows: vec![],
            pool_settings_point: None,
            offset: 0,
            progress: Progress::New,
        })
    }
    async fn clock(&mut self) -> Result<(String, bool)> {
        clock(&mut self.io).await
    }
    async fn read_families(
        &mut self,
        families: &[point::Family],
        metadata_visible: bool,
    ) -> Result<Vec<StateSourceCoverage>> {
        let mut outcomes = Vec::new();
        let tuning_layout = if families.iter().any(|family| matches!(family, point::Family::Tuning(_))) {
            self.admit().await?;
            let result = crate::query_store::tuning::Layout::read(&mut self.io).await;
            if let Err(error) = &result
                && stops_remaining_reads(error)
            {
                return Err(error.clone());
            }
            Some(result)
        } else {
            None
        };
        for family in families {
            let source = family.source().name.to_string();
            match family.availability(metadata_visible, tuning_layout.as_ref(), self.identity.as_ref()) {
                Ok(point::Availability::NotApplicable) => {
                    outcomes.push(StateSourceCoverage::not_applicable(source));
                    continue;
                }
                Ok(point::Availability::MissingMetadata) => {
                    outcomes.push(StateSourceCoverage::unavailable(source, ErrorCode::PermissionDenied, None));
                    continue;
                }
                Err(error) => {
                    outcomes.push(source_failure(source, error)?);
                    continue;
                }
                Ok(point::Availability::Available) => {}
            }
            let remaining = self.admit().await?;
            let result = family
                .read(
                    &mut self.io,
                    remaining.min(MAX_BUFFERED_FAMILY_ROWS),
                    tuning_layout.as_ref(),
                    self.identity.as_ref(),
                )
                .await;
            if let Ok(point::ReadOutcome::Observed(batch)) = &result
                && let Some(point) = &batch.pool_settings_point
            {
                if self.pool_settings_point.replace(point.clone()).is_some() {
                    return Err(invalid_data());
                }
            }
            let outcome = record_family(*family, result, remaining, &mut self.rows)?;
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }
    async fn admit(&mut self) -> Result<usize> {
        let remaining = (self.request.budget.max_snapshot_rows as usize).saturating_sub(self.rows.len());
        if remaining == 0 {
            return Err(CollectionStop::RowLimit.error());
        }
        // Ordinary instance statistics are an explicitly selected bounded source family. They do not execute
        // application work or optional enrichment; Azure database reads retain their native pressure guard.
        self.identity.as_ref().ok_or_else(invalid_data)?.admit(&mut self.io).await?;
        Ok(remaining)
    }
}
/// One family-result policy for current native point sources. A protocol/admission fence stops the
/// original owner; other failures keep this family's unavailability without erasing independent facts.
pub(super) fn record_family(
    family: point::Family,
    result: Result<point::ReadOutcome>,
    remaining: usize,
    rows: &mut Vec<TelemetryRecord>,
) -> Result<StateSourceCoverage> {
    let source = family.source().name.to_owned();
    match result {
        Ok(point::ReadOutcome::NotApplicable) => Ok(StateSourceCoverage::not_applicable(source)),
        Ok(point::ReadOutcome::Observed(batch)) => {
            if batch.values.len() > remaining {
                return Err(CollectionStop::RowLimit.error());
            }
            let count = batch.values.len().try_into().map_err(|_| invalid_data())?;
            let mut outcome = StateSourceCoverage::observed(source, count, batch.complete_scope);
            outcome.error_code = batch.error_code;
            rows.extend(batch.values);
            Ok(outcome)
        }
        Err(error) => source_failure(source, error),
    }
}
pub(super) fn source_failure(source: String, error: culprit_core::Error) -> Result<StateSourceCoverage> {
    let stopped = CollectionStop::from_error(&error);
    if stops_remaining_reads(&error) {
        return Err(error);
    }
    Ok(StateSourceCoverage::unavailable(source, error.code, Some(stopped)))
}
#[async_trait::async_trait]
impl<R: NativeRead> TelemetrySource for StateSource<R> {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        if self.progress != Progress::New {
            return Err(invalid_data());
        }
        self.progress = Progress::Failed;
        let dataset = pool_settings::dataset(&self.request.selection)?;
        let shared = subject::is_shared(&self.request.subject)?;
        let families = if pool_current::selected(&self.request) {
            vec![point::Family::WaitCounters]
        } else if pool_settings::selected(&self.request) {
            vec![point::Family::PoolSettings]
        } else if shared {
            point::shared_families(dataset)?
        } else {
            point::families(dataset)?
        };
        let detail = if !shared && dataset == SnapshotDataset::Operational {
            crate::database_identity::log_rates::Detail::ConfiguredLogRates
        } else {
            crate::database_identity::log_rates::Detail::Identity
        };
        let read = if pool_current::selected(&self.request) {
            pool_current::subject(&mut self.io, &self.request.subject).await?
        } else if pool_settings::selected(&self.request) {
            pool_settings::read_subject(&mut self.io, &self.request.subject, &self.host, self.port).await?
        } else {
            subject::read_detail(
                &mut self.io,
                &self.request.subject,
                &self.host,
                self.port,
                detail,
                self.database_authority,
            )
            .await?
        };
        if dataset == SnapshotDataset::Settings {
            settings::require_profile(&read.identity)?;
        }
        if matches!(dataset, SnapshotDataset::DatabasePerformanceCounters | SnapshotDataset::SuspectPages) {
            local_point::require_profile(&read.identity)?;
        }
        if dataset == SnapshotDataset::ProcessCpu
            && !matches!(&read.identity,subject::Identity::Instance(owner) if [2,3,4].contains(&owner.engine_edition()))
        {
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::Unsupported,
                culprit_core::t!("collection.unsupported"),
            ));
        }
        let target = read.identity.target();
        if matches!(dataset, SnapshotDataset::NumaNodes | SnapshotDataset::ProcessCpu) {
            self.point_generation = Some(
                self.io
                    .connection_generation()
                    .filter(|generation| *generation > 0)
                    .ok_or_else(|| CollectionStop::SourceUnavailable.error())?,
            );
        }
        self.identity = Some(read.identity);
        let (mut observed_from, metadata_visible, properties) = if !shared && dataset == SnapshotDataset::Operational {
            let read = crate::database_properties::read(&mut self.io).await?;
            if target.database().is_none_or(|database| database.database != read.database) {
                return Err(CollectionStop::SourceChanged.error());
            }
            let source = match read.properties {
                Ok(point) => {
                    if self.rows.len() >= self.request.budget.max_snapshot_rows as usize {
                        return Err(CollectionStop::RowLimit.error());
                    }
                    self.rows.push(TelemetryRecord::Operational(crate::database_properties::observation(point)?));
                    StateSourceCoverage::collected(DATABASE_PROPERTIES_SOURCE.into(), 1)
                }
                Err(error) => source_failure(DATABASE_PROPERTIES_SOURCE.into(), error)?,
            };
            (canonical_time(&read.observed_at)?, read.metadata_visible, Some(source))
        } else {
            let (at, visible) = self.clock().await?;
            (at, visible, None)
        };
        let mut sources = self.read_families(&families, metadata_visible).await?;
        if detail == crate::database_identity::log_rates::Detail::ConfiguredLogRates {
            sources.push(match read.configured_log_rates {
                Ok(Some(point)) => {
                    if let Some(at) = &point.observed_at
                        && canonical_time(at)? < observed_from
                    {
                        observed_from = at.clone();
                    }
                    if self.rows.len() >= self.request.budget.max_snapshot_rows as usize {
                        return Err(CollectionStop::RowLimit.error());
                    }
                    self.rows.push(TelemetryRecord::Operational(OperationalObservation {
                        source: CONFIGURED_LOG_RATES_SOURCE.into(),
                        key: point.key()?,
                        fact: OperationalFact::ConfiguredLogRateLimits(Box::new(point)),
                    }));
                    StateSourceCoverage::observed(CONFIGURED_LOG_RATES_SOURCE.into(), 1, false)
                }
                Ok(None) => {
                    StateSourceCoverage::unavailable(CONFIGURED_LOG_RATES_SOURCE.into(), ErrorCode::Unsupported, None)
                }
                Err(error) => source_failure(CONFIGURED_LOG_RATES_SOURCE.into(), error)?,
            });
        }
        let closing =
            pool_current::closing(self.identity.as_ref().ok_or_else(invalid_data)?, &mut self.io, &observed_from)
                .await?;
        let (observed_until, final_metadata_visible, pool_authority) = match closing {
            Some((at, visible, authority)) => (at, visible, Some(authority)),
            None => {
                let (at, visible) = self.clock().await?;
                (at, visible, None)
            }
        };
        if !final_metadata_visible {
            for (source, family) in sources.iter_mut().zip(&families) {
                if family.requires_metadata()
                    && matches!(source.state, culprit_core::SourceState::Collected | culprit_core::SourceState::Empty)
                {
                    source.state = culprit_core::SourceState::Partial;
                    source.complete_scope = false;
                    source.error_code = Some(ErrorCode::PermissionDenied);
                }
            }
        }
        if let Some(source) = properties {
            sources.push(source);
        }
        if !self.identity.as_ref().ok_or_else(invalid_data)?.accepts_observation_bounds(&observed_from, &observed_until)
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let description = CollectionDescription {
            partition: self.identity.as_ref().ok_or_else(invalid_data)?.partition()?,
            target,
            source: if pool_current::selected(&self.request) {
                MSSQL_POOL_CURRENT_SOURCE
            } else if dataset == SnapshotDataset::ProcessCpu {
                MSSQL_CPU_PROFILE_SOURCE
            } else if dataset == SnapshotDataset::NumaNodes {
                MSSQL_NUMA_NODES_SOURCE
            } else if pool_settings::selected(&self.request) {
                MSSQL_POOL_SETTINGS_SOURCE
            } else if shared {
                if dataset == SnapshotDataset::MemoryFailures {
                    "shared-memory-failure-state"
                } else {
                    "shared-operational-state"
                }
            } else {
                match dataset {
                    SnapshotDataset::Operational => "operational-state",
                    SnapshotDataset::DatabasePerformanceCounters => "database-performance-counter-state",
                    SnapshotDataset::SuspectPages => "suspect-page-history-state",
                    SnapshotDataset::WaitCounters => "wait-counter-state",
                    SnapshotDataset::Replication => "replication-state",
                    SnapshotDataset::Settings => "settings-state",
                    SnapshotDataset::PlanCacheSummary => "plan-cache-state",
                    SnapshotDataset::MissingIndexes => "missing-index-state",
                    SnapshotDataset::Tuning => "tuning-state",
                    SnapshotDataset::ProcessCpu
                    | SnapshotDataset::NumaNodes
                    | SnapshotDataset::HyperscaleLogComponents
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
                        return Err(invalid_data());
                    }
                    SnapshotDataset::MemoryFailures
                    | SnapshotDataset::NativeFailures
                    | SnapshotDataset::Jobs
                    | SnapshotDataset::ColumnstoreRowgroups
                    | SnapshotDataset::TableMemory
                    | SnapshotDataset::GroupReplication
                    | SnapshotDataset::Galera
                    | SnapshotDataset::FullTextPopulations
                    | SnapshotDataset::Cdc
                    | SnapshotDataset::ChangeFeed
                    | SnapshotDataset::BackupHistory
                    | SnapshotDataset::SessionMemory
                    | SnapshotDataset::PageWaitTargets
                    | SnapshotDataset::TaskSpace
                    | SnapshotDataset::PageWaitWriters
                    | SnapshotDataset::PageTaskHolders => return Err(invalid_data()),
                }
            }
            .into(),
            continuity: SourceContinuity::Unverified,
            observation: pool_settings::observation(
                StateSnapshotDescription {
                    dataset,
                    observed_from,
                    observed_until,
                    records: self.rows.len() as u32,
                    sources,
                },
                self.identity.as_ref().ok_or_else(invalid_data)?,
                self.pool_settings_point.clone(),
            )?,
        };
        let mut description = description;
        if let Some(authority) = pool_authority {
            let snapshot = description.observation.snapshot().ok_or_else(invalid_data)?.clone();
            description.observation = SourceObservation::MssqlPoolCurrent { snapshot, authority: Box::new(authority) };
            mssql_pool_current_authority(&description)?;
        }
        if !description.matches_request(&self.request) {
            return Err(invalid_data());
        }
        self.description = Some(description.clone());
        self.progress = Progress::Ready;
        Ok(description)
    }
    fn query_count(&self) -> u32 {
        self.io.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.read_metrics()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        if !matches!(self.progress, Progress::Ready | Progress::Reading) {
            return Err(invalid_data());
        }
        self.progress = Progress::Failed;
        let snapshot = self
            .description
            .as_ref()
            .and_then(|description| description.observation.snapshot())
            .ok_or_else(invalid_data)?;
        let expected = if self.offset == 0 { None } else { Some(self.offset.to_string()) };
        if stream != snapshot.dataset.stream() || cursor != expected.as_deref() {
            return Err(invalid_data());
        }
        let mut records = vec![];
        let mut bytes = 0usize;
        while self.offset < self.rows.len() && records.len() < self.request.budget.page_rows as usize {
            let record = &self.rows[self.offset];
            let size = serde_json::to_vec(record).map_err(|_| invalid_data())?.len() + ENVELOPE_ALLOWANCE;
            if bytes + size > self.request.budget.page_bytes as usize {
                if records.is_empty() {
                    return Ok(CollectionRead::Stopped(CollectionStop::ByteLimit));
                }
                break;
            }
            bytes += size;
            records.push(record.clone());
            self.offset += 1;
        }
        let complete = self.offset == self.rows.len();
        self.progress = if complete { Progress::Complete } else { Progress::Reading };
        Ok(CollectionRead::Page { records, next_cursor: (!complete).then(|| self.offset.to_string()) })
    }
    async fn finish(&mut self) -> Result<culprit_core::telemetry::SourceCompletion> {
        if self.progress != Progress::Complete {
            return Err(invalid_data());
        }
        self.progress = Progress::Failed;
        let owner = self.identity.as_ref().ok_or_else(invalid_data)?;
        let (identity, final_at) = if self.point_generation.is_some() {
            owner.recheck_clock(&mut self.io, &self.host, self.port).await?
        } else {
            (owner.recheck(&mut self.io, &self.host, self.port).await?, None)
        };
        let until = &self
            .description
            .as_ref()
            .and_then(|source| source.observation.snapshot())
            .ok_or_else(invalid_data)?
            .observed_until;
        let description = self.description.as_ref().ok_or_else(invalid_data)?;
        let confirmed = self.point_generation.is_none_or(|generation| {
            self.io.connection_generation() == Some(generation)
                && final_at.as_deref().is_some_and(|at| ObservationBounds::new(until, at).is_some())
        }) && self.identity.as_ref().ok_or_else(invalid_data)?.confirmed_by(&identity, until)
            && pool_settings::confirmed_by(description, &identity);
        self.progress = Progress::Finished;
        Ok(culprit_core::telemetry::SourceCompletion::verified_if(confirmed))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.progress = Progress::Closed;
        self.io.close().await
    }
}

#[cfg(test)]
mod memory_failure_tests;
#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod shared_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod numa_node_tests;
