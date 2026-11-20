//! Explicit selected catalog capture, using the shared native owner and existing frozen paging/close lifecycle.
use super::state;
use crate::{
    columnstore_rowgroups as native,
    conn::Pool,
    local_source::Context,
    read::{NativeRead, ReadError, ReadSession, invalid_data},
};
use culprit_core::{
    Error, ErrorCode, Result,
    serialization::{JsonSizeError, json_size},
    telemetry::*,
};
use std::{collections::BTreeSet, sync::Arc};

const MAX_QUERIES: u32 = 11;
pub(super) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_queries: requested.max_queries.min(MAX_QUERIES),
        max_snapshot_rows: requested.max_snapshot_rows.min(MAX_COLUMNSTORE_ROWGROUPS),
        page_rows: requested.page_rows.min(32),
        page_bytes: requested.page_bytes.min(64 * 1024),
        total_bytes: requested.total_bytes.min(512 * 1024),
        max_pages: requested.max_pages.min(128),
        statement_timeout_ms: requested.statement_timeout_ms.min(1_000),
        total_timeout_ms: requested.total_timeout_ms.min(5_000),
        ..requested.clone()
    }
}
pub(super) struct RowgroupSource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<RowgroupReader<R>>,
}
struct RowgroupReader<R: NativeRead> {
    io: R,
    host: String,
    port: u16,
    target: MssqlColumnstoreTarget,
    original: Option<(Context, native::TargetPoint)>,
}
impl RowgroupSource {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        Self::from_reader(ReadSession::new(pool, bounded_budget(&request.budget)), request, host, port)
    }
}
impl<R: NativeRead> RowgroupSource<R> {
    fn from_reader(io: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        request.subject.require_database()?;
        let CollectionSelection::ColumnstoreRowgroups { target } = &request.selection else {
            return Err(invalid_data());
        };
        let target = target.as_ref().clone();
        let mut request = request.clone();
        request.budget = bounded_budget(&request.budget);
        request.budget.validate()?;
        let reader = RowgroupReader { io, host, port, target, original: None };
        Ok(Self { inner: culprit_read::FrozenSource::new(reader, &request)? })
    }
}
fn require_owner(context: &Context, target: &MssqlColumnstoreTarget) -> Result<()> {
    let activity = &context.activity;
    if !(activity.major >= 13 || matches!(activity.edition, 5 | 8)) {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    if !context.is_qualified()
        || !context.source_partition()?.is_qualified()
        || context.connection_generation().is_none_or(|generation| generation == 0)
    {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    if context.database_id != target.table.database_id {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(())
}
#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for RowgroupReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let before = Context::read(&mut self.io, &self.host, self.port).await?;
        require_owner(&before, &self.target)?;
        let selected = native::target(&mut self.io, &self.target).await?;
        if ObservationBounds::new(&before.activity.at, &selected.observed_at).is_none() {
            return Err(CollectionStop::SourceChanged.error());
        }
        if before.activity.edition == crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
            crate::resources::admit_resources(&mut self.io).await
        } else {
            crate::resources::admit_instance_resources(&mut self.io).await
        }
        .map_err(ReadError::into_error)?;
        let limit = request.budget.max_snapshot_rows;
        let points = native::read(&mut self.io, &self.target, selected.index_type, limit).await?;
        let (until, _) = state::clock(&mut self.io).await?;
        let bounds = ObservationBounds::new(&selected.observed_at, &until)
            .ok_or_else(|| CollectionStop::SourceChanged.error())?;
        if points.iter().any(|point| !bounds.contains(&point.observed_at)) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let context = NativeReadContext {
            target: before.activity.database.clone().into(),
            partition: before.source_partition()?,
            observed_from: before.activity.at.clone(),
            observed_until: until,
            database_writable: before.activity.writable,
        };
        let mut records =
            points.into_iter().map(|point| point.bind(&context, &self.target)).collect::<Result<Vec<_>>>()?;
        let mut keys = BTreeSet::new();
        if !records
            .iter()
            .all(|record| matches!(record, TelemetryRecord::Operational(row) if keys.insert(row.key.clone())))
        {
            return Err(invalid_data());
        }
        let complete = records.len() <= limit as usize;
        records.truncate(limit as usize);
        let mut coverage =
            StateSourceCoverage::observed(MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(), records.len() as u32, complete);
        if !complete {
            coverage.stopped = Some(CollectionStop::RowLimit);
        }
        let description = CollectionDescription {
            target: context.target,
            partition: context.partition,
            source: MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::ColumnstoreRowgroups {
                target: Box::new(self.target.clone()),
                index_type: selected.index_type,
                database_writable: context.database_writable.ok_or_else(invalid_data)?,
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::ColumnstoreRowgroups,
                    observed_from: context.observed_from,
                    observed_until: context.observed_until,
                    records: records.len() as u32,
                    sources: vec![coverage],
                },
            },
        };
        json_size(&(&description, &records), request.budget.total_bytes as usize).map_err(|error| match error {
            JsonSizeError::LimitExceeded => CollectionStop::ByteLimit.error(),
            JsonSizeError::Serialization => invalid_data(),
        })?;
        self.original = Some((before, selected));
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        let (before, selected) = self.original.as_ref().ok_or_else(invalid_data)?;
        let final_target = native::target(&mut self.io, &self.target).await?;
        let until = &description.observation.snapshot().ok_or_else(invalid_data)?.observed_until;
        if final_target.index_type != selected.index_type
            || ObservationBounds::new(until, &final_target.observed_at).is_none()
        {
            return Ok(false);
        }
        let after = before.recheck(&mut self.io, &self.host, self.port).await?;
        require_owner(&after, &self.target)?;
        Ok(before.same_source(&after)
            && before.database_wide == after.database_wide
            && before.completed(&after).is_ok()
            && ObservationBounds::new(&final_target.observed_at, &after.activity.at).is_some())
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
impl<R: NativeRead> TelemetrySource for RowgroupSource<R> {
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
