//! Native recipes over shared snapshot delivery and PostgreSQL-wire ownership. No second scheduler or destination.
mod activity;
#[cfg(test)]
mod native_tests;
mod operational;
use crate::native::{
    self,
    metadata::{self, Metadata},
    schema, statements,
};
use culprit_core::{Engine, Error, ErrorCode, Result, telemetry::*};
use culprit_postgres::wire::{Connector, NativeRead, ReadSession, Row, catalog};
use culprit_read::{ReadBatch, SnapshotReader, SnapshotRecheck, SnapshotRecord, SnapshotScope, SnapshotSource};
#[derive(Clone, Copy)]
enum Recipe {
    Statements,
    Definitions,
    Plans,
    Schema(schema::Kind),
}
impl Recipe {
    fn dataset(self) -> SnapshotDataset {
        match self {
            Self::Statements => SnapshotDataset::StatementCounters,
            Self::Definitions => SnapshotDataset::StatementDefinitions,
            Self::Plans => SnapshotDataset::RetainedPlans,
            Self::Schema(kind) => kind.dataset(),
        }
    }
    fn source(self) -> &'static str {
        match self {
            Self::Statements | Self::Definitions => statements::SOURCE,
            Self::Plans => native::plans::SOURCE,
            Self::Schema(kind) => kind.source(),
        }
    }
    fn query(self, meta: &Metadata, layout: Option<&statements::Layout>, budget: &CollectionBudget) -> Result<String> {
        let limit = budget.max_snapshot_rows as usize + 1;
        match self {
            Self::Statements | Self::Definitions => Ok(layout.ok_or_else(native::invalid)?.query(
                meta,
                matches!(self, Self::Definitions),
                limit,
                budget.max_sql_bytes as usize,
            )),
            Self::Schema(kind) => Ok(kind.query(meta, limit)),
            Self::Plans => native::plans::query(meta, limit, budget.max_plan_bytes as usize),
        }
    }
    fn decode(
        self,
        row: &Row,
        meta: &Metadata,
        layout: Option<&statements::Layout>,
        source: &CollectionDescription,
        budget: &CollectionBudget,
    ) -> Result<TelemetryRecord> {
        match self {
            Self::Schema(kind) => kind.decode(row, meta, budget.max_sql_bytes as usize),
            Self::Plans => native::plans::decode(row, meta, source, budget.max_plan_bytes as usize),
            Self::Statements => {
                let sample = statements::counters(row, layout.ok_or_else(native::invalid)?, meta)?;
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: self.source().into(),
                    key: sample.key()?,
                    fact: OperationalFact::StatementCounters(Box::new(sample)),
                }))
            }
            Self::Definitions => Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
                self.source().into(),
                ObjectDefinitionFact::Statement(Box::new(statements::definition(
                    row,
                    meta,
                    source,
                    budget.max_sql_bytes as usize,
                )?)),
            )?)),
        }
    }
}
pub(crate) fn source(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    if request.collection() == TelemetryCollection::Activity {
        return activity::source(connector, request);
    }
    if request.collection() == TelemetryCollection::Snapshot(SnapshotDataset::Operational) {
        return operational::source(connector, request);
    }
    let recipe = match request.collection() {
        TelemetryCollection::Snapshot(dataset) => match dataset {
            SnapshotDataset::StatementCounters => Recipe::Statements,
            SnapshotDataset::StatementDefinitions => Recipe::Definitions,
            SnapshotDataset::RetainedPlans => Recipe::Plans,
            SnapshotDataset::Settings => Recipe::Schema(schema::Kind::Settings),
            SnapshotDataset::Tables => Recipe::Schema(schema::Kind::Tables),
            SnapshotDataset::Indexes => Recipe::Schema(schema::Kind::Indexes),
            SnapshotDataset::Columns => Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Columns)),
            SnapshotDataset::ForeignKeys => Recipe::Schema(schema::Kind::Catalog(catalog::Kind::ForeignKeys)),
            SnapshotDataset::CheckConstraints => Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Checks)),
            SnapshotDataset::Objects => Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Objects)),
            SnapshotDataset::Triggers => Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Triggers)),
            SnapshotDataset::Partitions => Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Partitions)),
            SnapshotDataset::Statistics => Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Statistics)),
            _ => return Err(unsupported()),
        },
        _ => return Err(unsupported()),
    };
    Ok(Box::new(SnapshotSource::new(
        Reader {
            host: connector.host.clone(),
            port: connector.port,
            io: ReadSession::for_engine(connector, request.budget.clone(), Engine::Yugabyte),
            recipe,
            metadata: None,
            layout: None,
            budget: request.budget.clone(),
            notices: 0,
        },
        request,
    )?))
}
fn unsupported() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))
}
struct Reader {
    io: ReadSession,
    host: String,
    port: u16,
    recipe: Recipe,
    metadata: Option<Metadata>,
    layout: Option<statements::Layout>,
    budget: CollectionBudget,
    notices: u64,
}
#[async_trait::async_trait]
impl SnapshotReader for Reader {
    type Row = Row;
    async fn describe(&mut self, _: &CollectionRequest) -> Result<CollectionDescription> {
        let meta = metadata::read(&mut self.io, &self.host, self.port).await?;
        if matches!(self.recipe, Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Statistics)))
            && !catalog::statistics_visible(&mut self.io).await?
        {
            return Err(Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source")));
        }
        if matches!(self.recipe, Recipe::Statements | Recipe::Definitions) {
            self.layout = Some(statements::Layout::read(&mut self.io, &meta).await?);
        }
        if self.io.notices() != 0 {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        self.notices = self.io.notices();
        let description = CollectionDescription {
            target: meta.database.clone().into(),
            source: format!("ysql-{}-{}", self.recipe.source(), meta.node),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PagedSnapshot(PagedSnapshotDescription {
                dataset: self.recipe.dataset(),
                observed_from: meta.probe.now.clone(),
                sources: vec![self.recipe.source().into()],
            }),
        };
        self.metadata = Some(meta);
        Ok(description)
    }
    async fn begin(&mut self, _: &CollectionRequest) -> Result<()> {
        let sql = self.recipe.query(
            self.metadata.as_ref().ok_or_else(native::invalid)?,
            self.layout.as_ref(),
            &self.budget,
        )?;
        self.io.begin_stream(&sql, self.budget.max_snapshot_rows as usize + 1).await
    }
    async fn next_batch(&mut self) -> Result<ReadBatch<Row>> {
        self.io.next_batch().await
    }
    fn decode(&self, row: &Row, source: &CollectionDescription) -> Result<SnapshotRecord> {
        Ok(SnapshotRecord {
            source: self.recipe.source().into(),
            record: self.recipe.decode(
                row,
                self.metadata.as_ref().ok_or_else(native::invalid)?,
                self.layout.as_ref(),
                source,
                &self.budget,
            )?,
            complete_scope: true,
        })
    }
    async fn recheck(&mut self, _: &CollectionDescription) -> Result<SnapshotRecheck> {
        let after = metadata::read(&mut self.io, &self.host, self.port).await?;
        let before = self.metadata.as_ref().ok_or_else(native::invalid)?;
        Ok(SnapshotRecheck {
            unchanged: before.same_source(&after)
                && (!matches!(self.recipe, Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Statistics)))
                    || catalog::statistics_visible(&mut self.io).await?),
            observed_until: after.probe.now,
            scopes: vec![SnapshotScope::Available {
                source: self.recipe.source().into(),
                complete: self.io.notices() == self.notices,
                error_code: None,
            }],
        })
    }
    fn query_count(&self) -> u32 {
        self.io.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}
