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
    const SOURCE_READ: Option<SourceReadQuestion> = None;
    async fn layout<R: NativeRead>(&self, reader: &mut R, budget: &CollectionBudget) -> Result<Option<Self::Layout>>;
    fn query(&self, layout: &Self::Layout) -> String;
    fn decode(&self, row: &Rec, layout: &Self::Layout, source: &CollectionDescription) -> Result<TelemetryRecord>;
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
    source_read: Option<SourceReadRequest>,
    local: Option<crate::local_source::Context>,
    layout: Option<D::Layout>,
}
struct SourceReadRequest {
    focus_key: String,
    execution: NativeSourceReadScope,
}

impl<D: SnapshotRecipe> StreamedSnapshot<D> {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16, recipe: D) -> Result<Self> {
        Self::with_io(ReadSession::new(pool, request.budget.clone()), request, host, port, recipe)
    }
}
impl<D: SnapshotRecipe, R: NativeStreamRead> StreamedSnapshot<D, R> {
    fn with_io(io: R, request: &CollectionRequest, host: String, port: u16, recipe: D) -> Result<Self> {
        request.validate()?;
        let source_read = match &request.selection {
            CollectionSelection::Snapshot { dataset } if *dataset == D::DATASET => None,
            CollectionSelection::SourceRead { focus, execution }
                if D::SOURCE_READ == Some(focus.question) && focus.question.dataset() == D::DATASET =>
            {
                Some(SourceReadRequest { focus_key: focus.key()?, execution: execution.as_ref().clone() })
            }
            _ => return Err(invalid_data()),
        };
        let reader = NativeSnapshotReader {
            recipe,
            io,
            budget: request.budget.clone(),
            host,
            port,
            physical: None,
            source_read,
            local: None,
            layout: None,
        };
        Ok(Self { inner: culprit_read::SnapshotSource::new(reader, request)? })
    }
}
#[async_trait::async_trait]
impl<D: SnapshotRecipe, R: NativeStreamRead> culprit_read::SnapshotReader for NativeSnapshotReader<D, R> {
    type Row = Rec;
    fn supports_source_read(&self, question: SourceReadQuestion) -> bool {
        D::SOURCE_READ == Some(question)
    }
    async fn describe(&mut self, _: &CollectionRequest) -> Result<CollectionDescription> {
        let (target, partition, observed_from, metadata_visible) = if let Some(expected) = &self.source_read {
            description::ensure_supported(&mut self.io).await?;
            // Replace the existing Azure identity projection with the held reader's native authority projection.
            // This does not add a query or turn updateability into Primary/Secondary identity.
            let (context, _) = crate::local_source::read_initial(
                &mut self.io,
                &self.host,
                self.port,
                crate::activity::IdentityDetail::AzureDatabase,
                crate::local_source::Detail::Authority,
            )
            .await?;
            let actual = context.completed(&context)?;
            if !expected.execution.matches_context(&actual) {
                return Err(CollectionStop::SourceChanged.error());
            }
            let visible = state::clock(&mut self.io).await?.1;
            let target = actual.target.clone();
            let observed_from = actual.observed_from.clone();
            self.local = Some(context);
            (target, SourcePartition::Target, observed_from, visible)
        } else if D::INSTANCE_LOCAL {
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
        let snapshot =
            PagedSnapshotDescription { dataset: D::DATASET, observed_from, sources: vec![D::SOURCE.name.into()] };
        let observation = match &self.source_read {
            Some(expected) => {
                let context = self.local.as_ref().ok_or_else(invalid_data)?;
                SourceObservation::PagedSourceRead {
                    snapshot,
                    focus_key: expected.focus_key.clone(),
                    execution: Box::new(context.completed(context)?),
                }
            }
            None => SourceObservation::PagedSnapshot(snapshot),
        };
        Ok(CollectionDescription {
            partition,
            target,
            source: D::NAME.into(),
            continuity: SourceContinuity::Unverified,
            observation,
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
    fn decode(&self, row: &Rec, source: &CollectionDescription) -> Result<culprit_read::SnapshotRecord> {
        let layout = self.layout.as_ref().ok_or_else(invalid_data)?;
        let record = self.recipe.decode(row, layout, source)?;
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
            let source_visibility =
                if self.source_read.is_some() { Some(state::clock(&mut self.io).await?.1) } else { None };
            let after = before.recheck(&mut self.io, &self.host, self.port).await?;
            let context = match before.completed(&after) {
                Ok(context) => context,
                Err(error) if CollectionStop::from_error(&error) == CollectionStop::SourceChanged => {
                    return Ok(culprit_read::SnapshotRecheck {
                        execution: None,
                        unchanged: false,
                        observed_until: String::new(),
                        scopes: vec![],
                    });
                }
                Err(error) => return Err(error),
            };
            let scope_matches = match &self.source_read {
                Some(expected) => {
                    initial.partition == SourcePartition::Target
                        && expected.execution.matches_target(&initial.target)
                        && expected.execution.matches_context(&context)
                }
                None => initial.target == context.target && initial.partition == context.partition,
            };
            if !scope_matches {
                return Err(CollectionStop::SourceChanged.error());
            }
            let visible = match source_visibility {
                Some(visible) => visible,
                None if D::REQUIRES_FULL_METADATA => state::clock(&mut self.io).await?.1,
                None => true,
            };
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
                observed_until: context.observed_until.clone(),
                execution: self.source_read.as_ref().map(|_| Box::new(context)),
                scopes: vec![scope],
            });
        }
        let (database, physical) = description::identity(&mut self.io, &self.host, self.port).await?;
        if initial.target.database() != Some(&database) || self.physical.as_ref() != Some(&physical) {
            return Ok(culprit_read::SnapshotRecheck {
                execution: None,
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
        Ok(culprit_read::SnapshotRecheck { unchanged: true, observed_until, execution: None, scopes: vec![scope] })
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
