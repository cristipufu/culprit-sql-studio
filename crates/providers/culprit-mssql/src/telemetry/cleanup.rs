//! Local cleanup facts use the existing database readers and one held native owner. Paging and close
//! remain in FrozenSource; independently unavailable families retain their original coverage.
use super::{point, state};
use crate::{
    conn::Pool,
    local_source::Context,
    read::{NativeRead, ReadSession, invalid_data},
};
use culprit_core::{Result, telemetry::*};
use std::sync::Arc;

const FAMILIES: [point::Family; 3] =
    [point::Family::CleanupHorizon, point::Family::VersionStore, point::Family::PersistentVersions];
const MAX_RECORDS: u32 = FAMILIES.len() as u32;
// Before/final authority use platform + process + database on this same connection. The family work
// is horizon + tempdb version-store + PVS layout/ADR/optional stats, followed by one native clock.
const AUTHORITY_QUERIES: u32 = 3;
const MAX_FAMILY_QUERIES: u32 = 1 + 1 + 3;
const SNAPSHOT_CLOCK_QUERIES: u32 = 1;
pub(super) const MAX_QUERIES: u32 = 2 * AUTHORITY_QUERIES + MAX_FAMILY_QUERIES + SNAPSHOT_CLOCK_QUERIES;
const MAX_PAGE_BYTES: u32 = 32 * 1024;

pub(super) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_snapshot_rows: requested.max_snapshot_rows.min(MAX_RECORDS),
        page_rows: requested.page_rows.min(MAX_RECORDS),
        page_bytes: requested.page_bytes.min(MAX_PAGE_BYTES),
        total_bytes: requested.total_bytes.min(MAX_PAGE_BYTES),
        max_pages: requested.max_pages.min(MAX_RECORDS),
        max_queries: requested.max_queries.min(MAX_QUERIES),
        statement_timeout_ms: requested.statement_timeout_ms.min(1_000),
        total_timeout_ms: requested.total_timeout_ms.min(5_000),
        ..requested.clone()
    }
}

pub(super) struct CleanupSource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<CleanupReader<R>>,
}
struct CleanupReader<R: NativeRead> {
    io: R,
    host: String,
    port: u16,
    context: Option<Context>,
}
impl CleanupSource {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        Self::with_io(ReadSession::new(pool, bounded_budget(&request.budget)), request, host, port)
    }
}
impl<R: NativeRead> CleanupSource<R> {
    fn with_io(io: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        request.subject.require_database()?;
        if request.selection != (CollectionSelection::Snapshot { dataset: SnapshotDataset::Cleanup }) {
            return Err(invalid_data());
        }
        let mut request = request.clone();
        request.budget = bounded_budget(&request.budget);
        request.budget.validate()?;
        let reader = CleanupReader { io, host, port, context: None };
        Ok(Self { inner: culprit_read::FrozenSource::new(reader, &request)? })
    }
}

#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for CleanupReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let context = Context::read(&mut self.io, &self.host, self.port).await?;
        require_local_instance(&context)?;
        let mut records = Vec::new();
        let mut sources = Vec::new();
        for family in FAMILIES {
            let remaining = (request.budget.max_snapshot_rows as usize).saturating_sub(records.len());
            if remaining == 0 {
                return Err(CollectionStop::RowLimit.error());
            }
            let result = family.read(&mut self.io, remaining, None, None).await;
            let mut coverage = state::record_family(family, result, remaining, &mut records)?;
            if matches!(family, point::Family::CleanupHorizon)
                && !context.database_wide
                && matches!(coverage.state, culprit_core::SourceState::Collected | culprit_core::SourceState::Empty)
            {
                coverage.state = culprit_core::SourceState::Partial;
                coverage.complete_scope = false;
            }
            sources.push(coverage);
        }
        let (until, _) = state::clock(&mut self.io).await?;
        if ObservationBounds::new(&context.activity.at, &until).is_none() {
            return Err(CollectionStop::SourceChanged.error());
        }
        let description = CollectionDescription {
            target: context.activity.database.clone().into(),
            partition: context.source_partition()?,
            source: "mssql-cleanup".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::Cleanup,
                observed_from: context.activity.at.clone(),
                observed_until: until,
                records: records.len().try_into().map_err(|_| invalid_data())?,
                sources,
            }),
        };
        self.context = Some(context);
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        let original = self.context.as_ref().ok_or_else(invalid_data)?;
        let after = original.recheck(&mut self.io, &self.host, self.port).await?;
        require_local_instance(&after)?;
        let until = &description.observation.snapshot().ok_or_else(invalid_data)?.observed_until;
        Ok(original.database_wide == after.database_wide
            && original.same_source(&after)
            && original.completed(&after).is_ok()
            && ObservationBounds::new(until, &after.activity.at).is_some())
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

fn require_local_instance(context: &Context) -> Result<()> {
    if !matches!(context.source_instance(), Some(member) if member.kind == SharedResourceKind::Instance) {
        return Err(culprit_core::Error::new(
            culprit_core::ErrorCode::Unsupported,
            culprit_core::t!("collection.unsupported"),
        ));
    }
    if !context.is_qualified() || context.connection_generation().is_none_or(|generation| generation == 0) {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    Ok(())
}

#[async_trait::async_trait]
impl<R: NativeRead> TelemetrySource for CleanupSource<R> {
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
mod native_tests;
#[cfg(test)]
mod tests;
