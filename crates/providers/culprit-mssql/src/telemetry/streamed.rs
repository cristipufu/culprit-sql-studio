//! Owns one streamed snapshot lifecycle. Recipes supply only native layout, query and decoding.
use super::{description, state};
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
    /// Connected-cache facts need a checked native compute member and process epoch, independent of history groups.
    const INSTANCE_LOCAL: bool;
    async fn layout<R: NativeRead>(&self, reader: &mut R, budget: &CollectionBudget) -> Result<Option<Self::Layout>>;
    fn query(&self, layout: &Self::Layout) -> String;
    fn decode(&self, row: &Rec, layout: &Self::Layout) -> Result<TelemetryRecord>;
    /// Retain bounded native witnesses while rows stream. The reader owns this state; decoders keep a read-only
    /// layout view. No additional SQL may run while the native stream is active.
    fn observe_batch(&self, _rows: &[Rec], _layout: &mut Self::Layout) -> Result<()> {
        Ok(())
    }
    /// Independent post-EOF proof required by a recipe's native relationships. Publication still requires the
    /// common target recheck and confirmed shutdown. A failure must stop before later source SQL.
    async fn recheck<R: NativeRead>(&self, _reader: &mut R, _layout: &Self::Layout) -> Result<()> {
        Ok(())
    }
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
    local: Option<crate::local_source::Context>,
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
            local: None,
            layout: None,
        };
        Ok(Self { inner: culprit_read::SnapshotSource::new(reader, request)? })
    }
}
#[async_trait::async_trait]
impl<D: SnapshotRecipe, R: NativeStreamRead> culprit_read::SnapshotReader for NativeSnapshotReader<D, R> {
    type Row = Rec;
    async fn describe(&mut self, _: &CollectionRequest) -> Result<CollectionDescription> {
        let (target, partition, observed_from, metadata_visible) = if D::INSTANCE_LOCAL {
            let context = crate::local_source::Context::read(&mut self.io, &self.host, self.port).await?;
            // Source locality does not expand this recipe's independently qualified platform or role policy.
            if context.activity.edition != crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
                return Err(culprit_core::Error::new(
                    culprit_core::ErrorCode::Unsupported,
                    culprit_core::t!("collection.azure_sql_only"),
                ));
            }
            crate::read::require_primary(context.activity.writable)?;
            let partition = context.source_partition()?;
            let observed_from = context.activity.at.clone();
            let target = context.activity.database.clone().into();
            let visible = if D::REQUIRES_FULL_METADATA { state::clock(&mut self.io).await?.1 } else { true };
            self.local = Some(context);
            (target, partition, observed_from, visible)
        } else {
            description::ensure_supported(&mut self.io).await?;
            let (database, physical) = description::identity(&mut self.io, &self.host, self.port).await?;
            let (at, visible) = state::clock(&mut self.io).await?;
            self.physical = Some(physical);
            (database.into(), SourcePartition::Target, at, visible)
        };
        if D::REQUIRES_FULL_METADATA && !metadata_visible {
            return Err(crate::catalog_visibility::unavailable());
        }
        crate::resources::admit_resources(&mut self.io).await.map_err(ReadError::into_error)?;
        self.layout = self.recipe.layout(&mut self.io, &self.budget).await?;
        Ok(CollectionDescription {
            partition,
            target,
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
        crate::resources::admit_resources(&mut self.io).await.map_err(ReadError::into_error)?;
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
        let batch = self.io.next_batch().await.map_err(ReadError::into_error)?;
        let layout = self.layout.as_mut().ok_or_else(invalid_data)?;
        self.recipe.observe_batch(&batch.rows, layout)?;
        Ok(batch)
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
        if let Some(layout) = &self.layout {
            self.recipe.recheck(&mut self.io, layout).await?;
        }
        if let Some(before) = &self.local {
            let after = before.recheck(&mut self.io, &self.host, self.port).await?;
            let context = match before.completed(&after) {
                Ok(context) => context,
                Err(error) if CollectionStop::from_error(&error) == CollectionStop::SourceChanged => {
                    return Ok(culprit_read::SnapshotRecheck {
                        unchanged: false,
                        observed_until: String::new(),
                        scopes: vec![],
                    });
                }
                Err(error) => return Err(error),
            };
            if initial.target != context.target || initial.partition != context.partition {
                return Err(CollectionStop::SourceChanged.error());
            }
            let visible = if D::REQUIRES_FULL_METADATA { state::clock(&mut self.io).await?.1 } else { true };
            let scope = if self.layout.is_none() {
                culprit_read::SnapshotScope::NotApplicable { source: D::SOURCE.name.into() }
            } else {
                culprit_read::SnapshotScope::Available {
                    source: D::SOURCE.name.into(),
                    complete: context.partition.is_qualified()
                        && before.database_wide
                        && after.database_wide
                        && visible,
                    error_code: (!visible).then_some(culprit_core::ErrorCode::PermissionDenied),
                }
            };
            return Ok(culprit_read::SnapshotRecheck {
                unchanged: true,
                observed_until: context.observed_until,
                scopes: vec![scope],
            });
        }
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
