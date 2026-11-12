//! Owns one streamed snapshot lifecycle. Recipes supply only native layout, query and decoding.
use super::{admit_resources, description, state};
use crate::{
    conn::Pool,
    read::{NativeStreamRead, Parameter, ReadError, ReadSession, invalid_data},
};
use crate::{read::NativeRead, value::Rec};
use culprit_core::{Result, Source, telemetry::*};

#[async_trait::async_trait]
pub(super) trait SnapshotRecipe: Send + Sync {
    type Layout: Send + Sync;
    const DATASET: SnapshotDataset;
    const NAME: &'static str;
    const SOURCE: &'static Source;
    const REQUIRES_FULL_METADATA: bool = false;
    async fn layout<R: NativeRead>(&self, reader: &mut R, budget: &CollectionBudget) -> Result<Option<Self::Layout>>;
    fn query(&self, layout: &Self::Layout) -> String;
    fn decode(&self, row: &Rec, layout: &Self::Layout) -> Result<TelemetryRecord>;
    fn complete_record_scope(&self, _record: &TelemetryRecord) -> bool {
        true
    }
}

use std::sync::Arc;

pub(super) struct StreamedSnapshot<D: SnapshotRecipe, R: NativeStreamRead = ReadSession> {
    inner: culprit_read::SnapshotSource<NativeSnapshotReader<D, R>>,
}
struct NativeSnapshotReader<D: SnapshotRecipe, R> {
    recipe: D,
    io: R,
    budget: CollectionBudget,
    host: String,
    port: u16,
    physical: Option<String>,
    layout: Option<D::Layout>,
}
impl<D: SnapshotRecipe> StreamedSnapshot<D> {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16, recipe: D) -> Result<Self> {
        Self::with_io(ReadSession::new(pool, request.budget.clone()), request, host, port, recipe)
    }
}
impl<D: SnapshotRecipe, R: NativeStreamRead> StreamedSnapshot<D, R> {
    fn with_io(io: R, request: &CollectionRequest, host: String, port: u16, recipe: D) -> Result<Self> {
        if request.selection != (CollectionSelection::Snapshot { dataset: D::DATASET }) {
            return Err(invalid_data());
        }
        let reader = NativeSnapshotReader {
            recipe,
            io,
            budget: request.budget.clone(),
            host,
            port,
            physical: None,
            layout: None,
        };
        Ok(Self { inner: culprit_read::SnapshotSource::new(reader, request)? })
    }
}
#[async_trait::async_trait]
impl<D: SnapshotRecipe, R: NativeStreamRead> culprit_read::SnapshotReader for NativeSnapshotReader<D, R> {
    type Row = Rec;
    async fn describe(&mut self, _: &CollectionRequest) -> Result<CollectionDescription> {
        description::ensure_supported(&mut self.io).await?;
        let (database, physical) = description::identity(&mut self.io, &self.host, self.port).await?;
        let (observed_from, metadata_visible) = state::clock(&mut self.io).await?;
        if D::REQUIRES_FULL_METADATA && !metadata_visible {
            return Err(crate::catalog_visibility::unavailable());
        }
        admit_resources(&mut self.io).await.map_err(ReadError::into_error)?;
        self.layout = self.recipe.layout(&mut self.io, &self.budget).await?;
        self.physical = Some(physical);
        Ok(CollectionDescription {
            target: database.into(),
            source: D::NAME.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PagedSnapshot(PagedSnapshotDescription {
                dataset: D::DATASET,
                observed_from,
                sources: vec![D::SOURCE.name.into()],
            }),
        })
    }
    async fn begin(&mut self, request: &CollectionRequest) -> Result<()> {
        let Some(layout) = &self.layout else { return Ok(()) };
        admit_resources(&mut self.io).await.map_err(ReadError::into_error)?;
        let limit = request.budget.max_snapshot_rows as usize + 1;
        self.io
            .begin_stream(&self.recipe.query(layout), vec![Parameter::Integer(limit as i64)], limit)
            .await
            .map_err(ReadError::into_error)
    }
    async fn next_batch(&mut self) -> Result<culprit_read::ReadBatch<Rec>> {
        if self.layout.is_none() {
            return Ok(culprit_read::ReadBatch { rows: vec![], complete: true });
        }
        self.io.next_batch().await.map_err(ReadError::into_error)
    }
    fn decode(&self, row: &Rec, _: &CollectionDescription) -> Result<culprit_read::SnapshotRecord> {
        let layout = self.layout.as_ref().ok_or_else(invalid_data)?;
        let record = self.recipe.decode(row, layout)?;
        Ok(culprit_read::SnapshotRecord {
            source: D::SOURCE.name.into(),
            complete_scope: self.recipe.complete_record_scope(&record),
            record,
        })
    }
    async fn recheck(&mut self, initial: &CollectionDescription) -> Result<culprit_read::SnapshotRecheck> {
        let (database, physical) = description::identity(&mut self.io, &self.host, self.port).await?;
        if initial.target.database() != Some(&database) || self.physical.as_ref() != Some(&physical) {
            return Ok(culprit_read::SnapshotRecheck {
                unchanged: false,
                observed_until: String::new(),
                scopes: vec![],
            });
        }
        let (observed_until, metadata_visible) = state::clock(&mut self.io).await?;
        let scope = if self.layout.is_none() {
            culprit_read::SnapshotScope::NotApplicable { source: D::SOURCE.name.into() }
        } else {
            let complete = !D::REQUIRES_FULL_METADATA || metadata_visible;
            culprit_read::SnapshotScope::Available {
                source: D::SOURCE.name.into(),
                complete,
                error_code: (!complete).then_some(culprit_core::ErrorCode::PermissionDenied),
            }
        };
        Ok(culprit_read::SnapshotRecheck { unchanged: true, observed_until, scopes: vec![scope] })
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
impl<D: SnapshotRecipe, R: NativeStreamRead> TelemetrySource for StreamedSnapshot<D, R> {
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
