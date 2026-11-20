//! One explicitly selected backup-metadata reader; original paging, verification and close remain FrozenSource's.
use super::state;
use crate::{
    backup_history::{self, Authority, Metadata, Point},
    conn::Pool,
    read::{NativeRead, ReadError, ReadSession, invalid_data, stops_remaining_reads},
};
use culprit_core::{Result, telemetry::*};
use std::sync::Arc;

// Boxed/MI: first observed-platform1 +shared authority3, metadata before/after/final3,
// admission1 +history1 +final shared authority3. Azure uses its existing logical/physical owner instead.
const MAX_QUERIES: u32 = 12;
const MAX_PAGE_BYTES: u32 = 256 * 1024;
const MAX_FRAME_BYTES: u32 = 512 * 1024;
pub(super) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_snapshot_rows: requested.max_snapshot_rows.min(MAX_MSSQL_BACKUP_HISTORY_ROWS),
        page_rows: requested.page_rows.min(16),
        page_bytes: requested.page_bytes.min(MAX_PAGE_BYTES),
        total_bytes: requested.total_bytes.min(MAX_FRAME_BYTES),
        max_pages: requested.max_pages.min(MAX_MSSQL_BACKUP_HISTORY_ROWS),
        max_queries: requested.max_queries.min(MAX_QUERIES),
        statement_timeout_ms: requested.statement_timeout_ms.min(1_000),
        total_timeout_ms: requested.total_timeout_ms.min(5_000),
        ..requested.clone()
    }
}
pub(super) struct BackupHistorySource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<BackupHistoryReader<R>>,
}
struct BackupHistoryReader<R: NativeRead> {
    io: R,
    host: String,
    port: u16,
    original: Option<(Authority, Metadata)>,
}
impl BackupHistorySource {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        Self::with_io(ReadSession::new(pool, bounded_budget(&request.budget)), request, host, port)
    }
}
impl<R: NativeRead> BackupHistorySource<R> {
    fn with_io(io: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        request.subject.require_database()?;
        if request.selection != (CollectionSelection::Snapshot { dataset: SnapshotDataset::BackupHistory }) {
            return Err(invalid_data());
        }
        let mut request = request.clone();
        request.budget = bounded_budget(&request.budget);
        request.budget.validate()?;
        let reader = BackupHistoryReader { io, host, port, original: None };
        Ok(Self { inner: culprit_read::FrozenSource::new(reader, &request)? })
    }
}
impl<R: NativeRead> BackupHistoryReader<R> {
    async fn read(&mut self, authority: &Authority, metadata: &Metadata, limit: u32) -> Result<Vec<Point>> {
        if let Some(error) = metadata.unavailable() {
            return Err(error);
        }
        let admission = if authority.profile() == MssqlBackupHistoryProfile::AzureSqlDatabase {
            crate::resources::admit_resources(&mut self.io).await
        } else {
            crate::resources::admit_instance_resources(&mut self.io).await
        };
        admission.map_err(ReadError::into_error)?;
        backup_history::read(&mut self.io, authority, metadata, limit).await
    }
}
#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for BackupHistoryReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let authority = Authority::read(&mut self.io, &self.host, self.port).await?;
        let before = backup_history::metadata(&mut self.io, &authority).await?;
        if ObservationBounds::new(authority.at(), &before.observed_at).is_none() {
            return Err(CollectionStop::SourceChanged.error());
        }
        let limit = request.budget.max_snapshot_rows.min(MAX_MSSQL_BACKUP_HISTORY_ROWS);
        let points = self.read(&authority, &before, limit).await;
        if let Err(error) = &points
            && stops_remaining_reads(error)
        {
            return Err(error.clone());
        }
        let after = backup_history::metadata(&mut self.io, &authority).await?;
        if !before.same_source(&after) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let context = NativeReadContext {
            target: authority.database().into(),
            partition: authority.partition()?,
            observed_from: authority.at().into(),
            observed_until: after.observed_at,
            database_writable: Some(authority.writable()?),
        };
        if !before.scope.matches_context(&context) {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        let (records, coverage) = match points {
            Ok(points) => {
                // Validate the complete returned roster, including lookahead, before retaining a bounded prefix.
                for point in &points {
                    point.validate(&context, &before.scope)?;
                }
                let overflow = points.len() > limit as usize;
                let records = points
                    .into_iter()
                    .take(limit as usize)
                    .enumerate()
                    .map(|(position, point)| {
                        point.bind(&context, &before.scope, position as u32 + 1).map(TelemetryRecord::Operational)
                    })
                    .collect::<Result<Vec<_>>>()?;
                let mut coverage =
                    StateSourceCoverage::observed(MSSQL_BACKUP_HISTORY_SOURCE.into(), records.len() as u32, false);
                if overflow {
                    coverage.stopped = Some(CollectionStop::RowLimit);
                }
                (records, coverage)
            }
            Err(error) => (Vec::new(), state::source_failure(MSSQL_BACKUP_HISTORY_SOURCE.into(), error)?),
        };
        let description = CollectionDescription {
            target: context.target,
            partition: context.partition,
            source: MSSQL_BACKUP_HISTORY_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::BackupHistory {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::BackupHistory,
                    observed_from: context.observed_from,
                    observed_until: context.observed_until,
                    records: records.len() as u32,
                    sources: vec![coverage],
                },
                scope: Box::new(before.scope.clone()),
                database_writable: authority.writable()?,
            },
        };
        backup_history_context(&description)?;
        self.original = Some((authority, before));
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        let (original, before) = self.original.as_ref().ok_or_else(invalid_data)?;
        let after = original.recheck(&mut self.io, &self.host, self.port).await?;
        if !original.same_source(&after) {
            return Ok(false);
        }
        let final_metadata = backup_history::metadata(&mut self.io, &after).await?;
        let snapshot = description.observation.snapshot().ok_or_else(invalid_data)?;
        Ok(before.same_source(&final_metadata)
            && ObservationBounds::new(&snapshot.observed_until, after.at()).is_some()
            && ObservationBounds::new(after.at(), &final_metadata.observed_at).is_some())
    }
    fn query_count(&self) -> u32 {
        self.io.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}
#[async_trait::async_trait]
impl<R: NativeRead> TelemetrySource for BackupHistorySource<R> {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.inner.describe().await
    }
    fn query_count(&self) -> u32 {
        self.inner.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.inner.read_metrics()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        self.inner.read_page(stream, cursor).await
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        self.inner.finish().await
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}
#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_tests;
