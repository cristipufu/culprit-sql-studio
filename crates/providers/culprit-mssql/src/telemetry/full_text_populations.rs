//! Manual selected runtime capture, owned by the existing native reader and FrozenSource paging/close lifecycle.
use super::state;
use crate::{
    conn::Pool,
    full_text_populations as native,
    local_source::Context,
    read::{NativeRead, ReadError, ReadSession, invalid_data},
};
use culprit_core::{
    Error, ErrorCode, Result,
    serialization::{JsonSizeError, json_size},
    telemetry::*,
};
use std::sync::Arc;

// Original/final local authority3+3, selected runtime1, admission1, clock1, target recheck1 and owned setup1.
const MAX_QUERIES: u32 = 12;
const NATIVE_STATUS_MAJOR: u32 = 11;
const SERVER_PERFORMANCE_STATE_MAJOR: u32 = 16;
pub(super) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_queries: requested.max_queries.min(MAX_QUERIES),
        max_snapshot_rows: requested.max_snapshot_rows.min(MAX_FULL_TEXT_POPULATIONS),
        page_rows: requested.page_rows.min(16),
        page_bytes: requested.page_bytes.min(64 * 1024),
        total_bytes: requested.total_bytes.min(256 * 1024),
        max_pages: requested.max_pages.min(64),
        statement_timeout_ms: requested.statement_timeout_ms.min(1_000),
        total_timeout_ms: requested.total_timeout_ms.min(5_000),
        ..requested.clone()
    }
}
// A fresh held session's SET batch consumes the same caller allowance. Reused sessions do not renew it.
fn reader_budget(requested: &CollectionBudget) -> Result<CollectionBudget> {
    ReadSession::budget_after_setup(bounded_budget(requested))
}
pub(super) struct FullTextPopulationSource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<PopulationReader<R>>,
}
struct PopulationReader<R: NativeRead> {
    io: R,
    host: String,
    port: u16,
    target: MssqlFullTextPopulationTarget,
    original: Option<Context>,
}
impl FullTextPopulationSource {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        Self::from_reader(ReadSession::new(pool, reader_budget(&request.budget)?), request, host, port)
    }
}
impl<R: NativeRead> FullTextPopulationSource<R> {
    fn from_reader(io: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        request.subject.require_database()?;
        let CollectionSelection::FullTextPopulations { target } = &request.selection else {
            return Err(invalid_data());
        };
        let target = target.as_ref().clone();
        let mut request = request.clone();
        request.budget = bounded_budget(&request.budget);
        request.budget.validate()?;
        let reader = PopulationReader { io, host, port, target, original: None };
        Ok(Self { inner: culprit_read::FrozenSource::new(reader, &request)? })
    }
}
fn require_owner(context: &Context, target: &MssqlFullTextPopulationTarget) -> Result<native::StatePermission> {
    let azure = context.activity.edition == crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE;
    if context.activity.major < NATIVE_STATUS_MAJOR
        || !(azure || crate::instance::supports_profile(context.activity.edition, context.activity.major))
    {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    if !context.is_qualified()
        || !context.source_partition()?.is_qualified()
        || context.connection_generation().is_none_or(|value| value == 0)
        || context.activity.writable.is_none()
    {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    if context.database_id != target.table.database_id {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(if context.activity.edition == crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
        native::StatePermission::AzureDatabase
    } else if context.activity.major >= SERVER_PERFORMANCE_STATE_MAJOR {
        native::StatePermission::ServerPerformanceState
    } else {
        native::StatePermission::ServerState
    })
}
#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for PopulationReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let before = Context::read(&mut self.io, &self.host, self.port).await?;
        let permission = require_owner(&before, &self.target)?;
        if before.activity.edition == crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
            crate::resources::admit_resources(&mut self.io).await
        } else {
            crate::resources::admit_instance_resources(&mut self.io).await
        }
        .map_err(ReadError::into_error)?;
        let read = native::read(&mut self.io, &self.target, request.budget.max_snapshot_rows, permission).await?;
        let (until, _) = state::clock(&mut self.io).await?;
        let bounds =
            ObservationBounds::new(&before.activity.at, &until).ok_or_else(|| CollectionStop::SourceChanged.error())?;
        if !bounds.contains(&read.point.metadata.observed_at) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let context = NativeReadContext {
            target: before.activity.database.clone().into(),
            partition: before.source_partition()?,
            observed_from: before.activity.at.clone(),
            observed_until: until,
            database_writable: before.activity.writable,
        };
        let metadata = Box::new(read.point.metadata.clone());
        let coverage = read.coverage.clone();
        let records = read.records(&context, &self.target)?;
        let description = CollectionDescription {
            target: context.target,
            partition: context.partition,
            source: MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::FullTextPopulations {
                metadata,
                target: Box::new(self.target.clone()),
                database_writable: context.database_writable.ok_or_else(invalid_data)?,
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::FullTextPopulations,
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
        self.original = Some(before);
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        let before = self.original.as_ref().ok_or_else(invalid_data)?;
        let target = native::target(&mut self.io, &self.target).await?;
        let until = &description.observation.snapshot().ok_or_else(invalid_data)?.observed_until;
        if ObservationBounds::new(until, &target.metadata.observed_at).is_none() {
            return Ok(false);
        }
        let after = before.recheck(&mut self.io, &self.host, self.port).await?;
        require_owner(&after, &self.target)?;
        Ok(before.same_source(&after)
            && before.database_wide == after.database_wide
            && before.completed(&after).is_ok()
            && ObservationBounds::new(&target.metadata.observed_at, &after.activity.at).is_some())
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
impl<R: NativeRead> TelemetrySource for FullTextPopulationSource<R> {
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
