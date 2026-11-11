//! Bounded current facts. Each native source retains its own availability and visibility contract.
use super::{admit_resources, description, point, progress::Progress};
use crate::{
    conn::Pool,
    read::{NativeRead, ReadError, ReadSession, integer, invalid_data, text},
};
use culprit_core::{ErrorCode, Result, telemetry::*};
use std::sync::Arc;
const CLOCK: &str = r#"SELECT /* culprit */ CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,
 HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS metadata_visibility"#;
const ENVELOPE_ALLOWANCE: usize = 4096;
const MAX_BUFFERED_FAMILY_ROWS: usize = 10_000;

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
    physical: Option<String>,
    rows: Vec<TelemetryRecord>,
    offset: usize,
    progress: Progress,
}
impl StateSource {
    pub(super) fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        Self::with_io(ReadSession::new(pool, request.budget.clone()), request, host, port)
    }
}
impl<R: NativeRead> StateSource<R> {
    fn with_io(io: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        Ok(Self {
            request: request.clone(),
            host,
            port,
            io,
            description: None,
            physical: None,
            rows: vec![],
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
        let mut blocked = None;
        let tuning_layout = if families.iter().any(|family| matches!(family, point::Family::Tuning(_))) {
            Some(match self.admit(&mut blocked).await {
                Ok(_) => crate::query_store::tuning::Layout::read(&mut self.io).await,
                Err(error) => Err(error),
            })
        } else {
            None
        };
        for family in families {
            let source = family.source().name.to_string();
            match family.availability(metadata_visible, tuning_layout.as_ref()) {
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
            let result = match self.admit(&mut blocked).await {
                Ok(remaining) => family
                    .read(&mut self.io, remaining.min(MAX_BUFFERED_FAMILY_ROWS), tuning_layout.as_ref())
                    .await
                    .and_then(|batch| {
                        if matches!(&batch, point::ReadOutcome::Observed(records) if records.values.len() > remaining) {
                            Err(CollectionStop::RowLimit.error())
                        } else {
                            Ok(batch)
                        }
                    }),
                Err(error) => Err(error),
            };
            let outcome = match result {
                Ok(point::ReadOutcome::NotApplicable) => StateSourceCoverage::not_applicable(source),
                Ok(point::ReadOutcome::Observed(batch)) => {
                    let count = batch.values.len().try_into().map_err(|_| invalid_data())?;
                    let mut outcome = StateSourceCoverage::observed(source, count, batch.complete_scope);
                    outcome.error_code = batch.error_code;
                    self.rows.extend(batch.values);
                    outcome
                }
                Err(error) => source_failure(source, error)?,
            };
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }
    async fn admit(&mut self, blocked: &mut Option<culprit_core::Error>) -> Result<usize> {
        if let Some(error) = blocked {
            return Err(error.clone());
        }
        let remaining = (self.request.budget.max_snapshot_rows as usize).saturating_sub(self.rows.len());
        if remaining == 0 {
            return Err(CollectionStop::RowLimit.error());
        }
        if let Err(error) = admit_resources(&mut self.io).await {
            let error = error.into_error();
            *blocked = Some(error.clone());
            return Err(error);
        }
        Ok(remaining)
    }
}
fn source_failure(source: String, error: culprit_core::Error) -> Result<StateSourceCoverage> {
    let stopped = CollectionStop::from_error(&error);
    if stopped == CollectionStop::CancellationUnconfirmed {
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
        let CollectionSelection::Snapshot { dataset } = self.request.selection else { return Err(invalid_data()) };
        let families = point::families(dataset)?;
        description::ensure_supported(&mut self.io).await?;
        let (database, physical) = description::identity(&mut self.io, &self.host, self.port).await?;
        let (observed_from, metadata_visible) = self.clock().await?;
        let mut sources = self.read_families(&families, metadata_visible).await?;
        let (observed_until, final_metadata_visible) = self.clock().await?;
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
        let description = CollectionDescription {
            database,
            source: match dataset {
                SnapshotDataset::Operational => "operational-state",
                SnapshotDataset::WaitCounters => "wait-counter-state",
                SnapshotDataset::Replication => "replication-state",
                SnapshotDataset::Settings => "settings-state",
                SnapshotDataset::PlanCacheSummary => "plan-cache-state",
                SnapshotDataset::MissingIndexes => "missing-index-state",
                SnapshotDataset::Tuning => "tuning-state",
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
                | SnapshotDataset::RetainedPlans => {
                    return Err(invalid_data());
                }
            }
            .into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset,
                observed_from,
                observed_until,
                records: self.rows.len() as u32,
                sources,
            }),
        };
        if !description.matches_request(&self.request) {
            return Err(invalid_data());
        }
        self.physical = Some(physical);
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
        let (database, physical) = description::identity(&mut self.io, &self.host, self.port).await?;
        self.progress = Progress::Finished;
        Ok(culprit_core::telemetry::SourceCompletion::verified_if(
            self.physical.as_ref() == Some(&physical)
                && self.description.as_ref().is_some_and(|before| before.database == database),
        ))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.progress = Progress::Closed;
        self.io.close().await
    }
}

#[cfg(test)]
mod tests;
