//! The explicitly selected local-memory profile delegates lifecycle and paging to FrozenSource.
use super::state;
use crate::{
    conn::Pool,
    local_source::Context,
    read::{NativeRead, ReadError, ReadSession, invalid_data},
    table_memory as native,
};
use culprit_core::{
    Error, ErrorCode, Result, SourceState,
    serialization::{JsonSizeError, json_size},
    telemetry::*,
};
use std::sync::Arc;

// Context3 + target1 + pressure1 + DMV/frame1 + clock1 + final target1/Context3 = 11 reads.
// The held session's single SET batch is separately reserved, never a renewed source allowance.
const MAX_QUERIES: u32 = 12;
const MAX_BYTES: u32 = 64 * 1024;
pub(super) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_queries: requested.max_queries.min(MAX_QUERIES),
        max_snapshot_rows: requested.max_snapshot_rows.min(1),
        page_rows: requested.page_rows.min(1),
        page_bytes: requested.page_bytes.min(MAX_BYTES),
        total_bytes: requested.total_bytes.min(MAX_BYTES),
        max_pages: requested.max_pages.min(1),
        statement_timeout_ms: requested.statement_timeout_ms.min(1_000),
        total_timeout_ms: requested.total_timeout_ms.min(5_000),
        ..requested.clone()
    }
}
fn reader_budget(requested: &CollectionBudget) -> Result<CollectionBudget> {
    ReadSession::budget_after_setup(bounded_budget(requested))
}
pub(super) struct TableMemorySource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<TableMemoryReader<R>>,
}
struct TableMemoryReader<R: NativeRead> {
    io: R,
    host: String,
    port: u16,
    target: MssqlObjectIdentity,
    original: Option<(Context, native::TargetPoint)>,
}
impl TableMemorySource {
    pub(super) fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        Self::from_reader(ReadSession::new(pool, reader_budget(&request.budget)?), request, host, port)
    }
}
impl<R: NativeRead> TableMemorySource<R> {
    fn from_reader(io: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        request.subject.require_database()?;
        let CollectionSelection::TableMemory { target } = &request.selection else { return Err(invalid_data()) };
        let mut bounded = request.clone();
        bounded.budget = bounded_budget(&request.budget);
        bounded.budget.validate()?;
        let reader = TableMemoryReader { io, host, port, target: target.as_ref().clone(), original: None };
        Ok(Self { inner: culprit_read::FrozenSource::new(reader, &bounded)? })
    }
}
fn require_owner(owner: &Context, target: &MssqlObjectIdentity) -> Result<()> {
    if !matches!((owner.activity.edition, owner.activity.major), (5 | 8, _) | (2 | 3 | 4, 16..)) {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    if !owner.is_qualified()
        || !owner.source_partition()?.is_qualified()
        || owner.connection_generation().is_none_or(|generation| generation == 0)
    {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    if owner.database_id != target.database_id {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(())
}
fn coverage(metadata: &MssqlTableMemoryMetadata, records: u32) -> StateSourceCoverage {
    if metadata.available() {
        return StateSourceCoverage::collected(MSSQL_TABLE_MEMORY_SOURCE.into(), records);
    }
    if metadata.memory_optimized == Some(false) {
        return StateSourceCoverage::not_applicable(MSSQL_TABLE_MEMORY_SOURCE.into());
    }
    let code = if metadata.dmv_permission == Some(false) || metadata.metadata_visible == Some(false) {
        ErrorCode::PermissionDenied
    } else if metadata.xtp_supported == Some(false) || !metadata.layout_available {
        ErrorCode::Unsupported
    } else {
        ErrorCode::QueryFailed
    };
    StateSourceCoverage::unavailable(MSSQL_TABLE_MEMORY_SOURCE.into(), code, None)
}
#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for TableMemoryReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let before = Context::read(&mut self.io, &self.host, self.port).await?;
        require_owner(&before, &self.target)?;
        let permission = native::permission(before.activity.major);
        let mut selected = native::target(&mut self.io, &self.target, permission).await?;
        if selected.writable() != before.activity.writable.ok_or_else(invalid_data)?
            || ObservationBounds::new(&before.activity.at, &selected.metadata.observed_at).is_none()
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let point = if selected.metadata.available() {
            if before.activity.edition == 5 {
                crate::resources::admit_resources(&mut self.io).await
            } else {
                crate::resources::admit_instance_resources(&mut self.io).await
            }
            .map_err(ReadError::into_error)?;
            let read = native::read(&mut self.io, &self.target, &selected, permission).await?;
            selected = read.selected;
            read.point
        } else {
            None
        };
        let (until, _) = state::clock(&mut self.io).await?;
        if ObservationBounds::new(&selected.metadata.observed_at, &until).is_none()
            || before.connection_generation() != self.io.connection_generation()
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let context = NativeReadContext {
            target: before.activity.database.clone().into(),
            partition: before.source_partition()?,
            observed_from: before.activity.at.clone(),
            observed_until: until,
            database_writable: before.activity.writable,
        };
        let records = point
            .into_iter()
            .map(|point| {
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: MSSQL_TABLE_MEMORY_SOURCE.into(),
                    key: point.key()?,
                    fact: OperationalFact::MssqlTableMemory(Box::new(point)),
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut outcome = coverage(&selected.metadata, records.len() as u32);
        if selected.metadata.available() && records.is_empty() {
            outcome.state = SourceState::Empty;
        }
        let description = CollectionDescription {
            target: context.target,
            partition: context.partition,
            source: MSSQL_TABLE_MEMORY_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::TableMemory {
                target: Box::new(self.target.clone()),
                metadata: Box::new(selected.metadata.clone()),
                database_writable: context.database_writable.ok_or_else(invalid_data)?,
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::TableMemory,
                    observed_from: context.observed_from,
                    observed_until: context.observed_until,
                    records: records.len() as u32,
                    sources: vec![outcome],
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
        let after_target =
            native::target(&mut self.io, &self.target, native::permission(before.activity.major)).await?;
        let until = &description.observation.snapshot().ok_or_else(invalid_data)?.observed_until;
        if !selected.same_object_state(&after_target)
            || ObservationBounds::new(until, &after_target.metadata.observed_at).is_none()
        {
            return Ok(false);
        }
        let after = before.recheck(&mut self.io, &self.host, self.port).await?;
        require_owner(&after, &self.target)?;
        Ok(before.same_source(&after)
            && before.completed(&after).is_ok()
            && ObservationBounds::new(&after_target.metadata.observed_at, &after.activity.at).is_some())
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
impl<R: NativeRead> TelemetrySource for TableMemorySource<R> {
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
