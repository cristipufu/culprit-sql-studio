//! Owns one streamed snapshot lifecycle. Recipes supply only native layout, query and decoding.
use super::{admit_resources, description, progress::Progress, state};
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

pub(super) struct StreamedSnapshot<D: SnapshotRecipe, R = ReadSession> {
    recipe: D,
    io: R,
    request: CollectionRequest,
    host: String,
    port: u16,
    description: Option<CollectionDescription>,
    physical: Option<String>,
    layout: Option<D::Layout>,
    progress: Progress,
    rows: u32,
    complete_scope: bool,
}
impl<D: SnapshotRecipe> StreamedSnapshot<D> {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16, recipe: D) -> Result<Self> {
        Self::with_io(ReadSession::new(pool, request.budget.clone()), request, host, port, recipe)
    }
}
impl<D: SnapshotRecipe, R: NativeStreamRead> StreamedSnapshot<D, R> {
    fn with_io(io: R, request: &CollectionRequest, host: String, port: u16, recipe: D) -> Result<Self> {
        request.validate()?;
        if request.selection != (CollectionSelection::Snapshot { dataset: D::DATASET }) {
            return Err(invalid_data());
        }
        Ok(Self {
            recipe,
            io,
            request: request.clone(),
            host,
            port,
            description: None,
            physical: None,
            layout: None,
            progress: Progress::New,
            rows: 0,
            complete_scope: true,
        })
    }
    async fn page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        if !matches!(self.progress, Progress::Ready | Progress::Reading) || stream != D::DATASET.stream() {
            return Err(invalid_data());
        }
        let expected = (self.rows > 0).then(|| self.rows.to_string());
        if cursor != expected.as_deref() {
            return Err(invalid_data());
        }
        let starting = self.progress == Progress::Ready;
        self.progress = Progress::Failed; // Dropping this future cannot authorize another native read.
        if starting {
            let Some(layout) = &self.layout else {
                self.progress = Progress::Complete;
                return Ok(CollectionRead::Page { records: vec![], next_cursor: None });
            };
            admit_resources(&mut self.io).await.map_err(ReadError::into_error)?;
            let limit = self.request.budget.max_snapshot_rows as usize + 1;
            self.io
                .begin_stream(&self.recipe.query(layout), vec![Parameter::Integer(limit as i64)], limit)
                .await
                .map_err(ReadError::into_error)?;
        }
        let batch = self.io.next_batch().await.map_err(ReadError::into_error)?;
        if batch.rows.is_empty() && !batch.complete {
            return Err(invalid_data());
        }
        let layout = self.layout.as_ref().ok_or_else(invalid_data)?;
        let records = batch.rows.iter().map(|row| self.recipe.decode(row, layout)).collect::<Result<Vec<_>>>()?;
        self.complete_scope &= records.iter().all(|record| self.recipe.complete_record_scope(record));
        self.rows =
            self.rows.checked_add(records.len().try_into().map_err(|_| invalid_data())?).ok_or_else(invalid_data)?;
        self.progress = if batch.complete { Progress::Complete } else { Progress::Reading };
        Ok(CollectionRead::Page { records, next_cursor: (!batch.complete).then(|| self.rows.to_string()) })
    }
}
#[async_trait::async_trait]
impl<D: SnapshotRecipe, R: NativeStreamRead> TelemetrySource for StreamedSnapshot<D, R> {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        if self.progress != Progress::New {
            return Err(invalid_data());
        }
        self.progress = Progress::Failed;
        description::ensure_supported(&mut self.io).await?;
        let (database, physical) = description::identity(&mut self.io, &self.host, self.port).await?;
        let (observed_from, metadata_visible) = state::clock(&mut self.io).await?;
        if D::REQUIRES_FULL_METADATA && !metadata_visible {
            return Err(crate::catalog_visibility::unavailable());
        }
        admit_resources(&mut self.io).await.map_err(ReadError::into_error)?;
        self.layout = self.recipe.layout(&mut self.io, &self.request.budget).await?;
        let description = CollectionDescription {
            database,
            source: D::NAME.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PagedSnapshot(PagedSnapshotDescription {
                dataset: D::DATASET,
                observed_from,
                sources: vec![D::SOURCE.name.into()],
            }),
        };
        if !description.matches_request(&self.request) {
            return Err(invalid_data());
        }
        self.description = Some(description.clone());
        self.physical = Some(physical);
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
        let result = self.page(stream, cursor).await;
        if result.is_err() {
            self.progress = Progress::Failed;
        }
        result
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        if self.progress != Progress::Complete {
            return Err(invalid_data());
        }
        self.progress = Progress::Failed;
        let (database, physical) = description::identity(&mut self.io, &self.host, self.port).await?;
        let initial = self.description.as_ref().ok_or_else(invalid_data)?;
        if initial.database != database || self.physical.as_ref() != Some(&physical) {
            self.progress = Progress::Finished;
            return Ok(SourceCompletion::Changed);
        }
        let SourceObservation::PagedSnapshot(declared) = &initial.observation else { return Err(invalid_data()) };
        let (observed_until, metadata_visible) = state::clock(&mut self.io).await?;
        let coverage = if self.layout.is_some() {
            let metadata_complete = !D::REQUIRES_FULL_METADATA || metadata_visible;
            let mut coverage = StateSourceCoverage::observed(
                D::SOURCE.name.into(),
                self.rows,
                self.complete_scope && metadata_complete,
            );
            if !metadata_complete {
                coverage.error_code = Some(culprit_core::ErrorCode::PermissionDenied);
            }
            coverage
        } else {
            StateSourceCoverage::not_applicable(D::SOURCE.name.into())
        };
        self.progress = Progress::Finished;
        Ok(SourceCompletion::Snapshot(StateSnapshotDescription {
            dataset: D::DATASET,
            observed_from: declared.observed_from.clone(),
            observed_until,
            records: self.rows,
            sources: vec![coverage],
        }))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.progress = Progress::Closed;
        self.io.close().await
    }
}

#[cfg(test)]
mod tests;
