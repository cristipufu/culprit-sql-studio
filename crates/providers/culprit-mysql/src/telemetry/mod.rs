//! Thin native recipes over the shared snapshot lifecycle. No archive, scheduler or destination ownership.
mod activity;
mod events;
#[cfg(test)]
mod native_tests;
use crate::{
    connect::{Connector, Scope},
    native::{
        self,
        metadata::{self, Metadata},
    },
    read::ReadSession,
    rows::Rec,
    vars::VariableSet,
};
use culprit_core::{Error, ErrorCode, MeasurementScope, Result, telemetry::*};
use culprit_read::{ReadBatch, SnapshotReader, SnapshotRecheck, SnapshotRecord, SnapshotScope, SnapshotSource};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Clone, Copy)]
enum Recipe {
    Settings,
    Statements,
    Definitions,
    Replication,
    Catalog(native::catalog::Kind),
    Operational(native::operational::Kind),
}
impl Recipe {
    fn dataset(self) -> SnapshotDataset {
        match self {
            Self::Settings => SnapshotDataset::Settings,
            Self::Statements => SnapshotDataset::StatementCounters,
            Self::Definitions => SnapshotDataset::StatementDefinitions,
            Self::Replication => SnapshotDataset::Replication,
            Self::Catalog(kind) => kind.dataset(),
            Self::Operational(kind) => kind.dataset(),
        }
    }
    fn source(self) -> &'static str {
        match self {
            Self::Settings => VariableSet::Configuration.source(),
            Self::Replication => native::replication::SOURCE,
            Self::Statements | Self::Definitions => native::statements::SOURCE,
            Self::Catalog(kind) => kind.source(),
            Self::Operational(kind) => kind.source(),
        }
    }
    fn query(
        self,
        meta: &Metadata,
        layout: Option<&native::statements::Layout>,
        budget: &CollectionBudget,
    ) -> Result<String> {
        match self {
            Self::Settings => Ok(VariableSet::Configuration.query(&crate::catalog::settings_names(meta.flavor))),
            Self::Replication => Ok(native::replication::query(meta.flavor, &meta.version).into()),
            Self::Catalog(kind) => Ok(kind.query(meta.flavor, &meta.version, budget.max_snapshot_rows as usize + 1)),
            Self::Operational(kind) => Ok(kind.query(meta.flavor, budget.max_snapshot_rows as usize + 1)),
            Self::Statements | Self::Definitions => layout.ok_or_else(native::invalid)?.query(
                matches!(self, Self::Definitions),
                budget.max_snapshot_rows as usize + 1,
                budget.max_sql_bytes as usize,
            ),
        }
    }
    fn decode(
        self,
        row: &Rec,
        meta: &Metadata,
        layout: Option<&native::statements::Layout>,
        source: &CollectionDescription,
        budget: &CollectionBudget,
    ) -> Result<TelemetryRecord> {
        match self {
            Self::Catalog(kind) => kind.decode(row, &meta.database_name),
            Self::Replication => native::replication::decode(row),
            Self::Operational(kind) => kind.decode(row, &meta.database_name),
            Self::Statements => {
                let sample =
                    native::statements::counters(row, layout.ok_or_else(native::invalid)?, &meta.database_name)?;
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: self.source().into(),
                    key: sample.key()?,
                    fact: OperationalFact::StatementCounters(Box::new(sample)),
                }))
            }
            Self::Definitions => Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
                self.source().into(),
                ObjectDefinitionFact::Statement(Box::new(native::statements::definition(
                    row,
                    &meta.database_name,
                    source,
                    budget.max_sql_bytes as usize,
                )?)),
            )?)),
            Self::Settings => Ok(TelemetryRecord::Setting(SettingObservation {
                source: self.source().into(),
                namespace: "global".into(),
                name: native::text(row, "variable_name")?.to_ascii_lowercase(),
                scope: MeasurementScope::Server,
                nature: SettingNature::Configuration,
                value: None,
                value_in_use: row.native_text("value")?,
                value_for_secondary: None,
                unit: None,
                native_context: Some("global".into()),
                native_source: None,
                pending_restart: None,
                native_is_default: None,
                default_value: None,
                default_source: SettingDefaultSource::Unknown,
            })),
        }
    }
}
pub(crate) fn source(connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    if request.collection() == TelemetryCollection::Activity {
        return activity::source(connector, request);
    }
    if request.collection() == TelemetryCollection::DeadlockHistory {
        return events::source(connector, request);
    }
    let recipe = match request.collection() {
        TelemetryCollection::Snapshot(SnapshotDataset::Settings) => Recipe::Settings,
        TelemetryCollection::Snapshot(SnapshotDataset::Replication) => Recipe::Replication,
        TelemetryCollection::Snapshot(SnapshotDataset::StatementCounters) => Recipe::Statements,
        TelemetryCollection::Snapshot(SnapshotDataset::StatementDefinitions) => Recipe::Definitions,
        TelemetryCollection::Snapshot(SnapshotDataset::Cleanup) => {
            Recipe::Operational(native::operational::Kind::Transactions)
        }
        TelemetryCollection::Snapshot(SnapshotDataset::Operational) => {
            Recipe::Operational(native::operational::Kind::Status)
        }
        TelemetryCollection::Snapshot(dataset) => match dataset {
            SnapshotDataset::Tables => Recipe::Catalog(native::catalog::Kind::Tables),
            SnapshotDataset::Indexes => Recipe::Catalog(native::catalog::Kind::Indexes),
            SnapshotDataset::Columns => Recipe::Catalog(native::catalog::Kind::Columns),
            SnapshotDataset::ForeignKeys => Recipe::Catalog(native::catalog::Kind::ForeignKeys),
            SnapshotDataset::CheckConstraints => Recipe::Catalog(native::catalog::Kind::Checks),
            SnapshotDataset::Objects => Recipe::Catalog(native::catalog::Kind::Objects),
            SnapshotDataset::Triggers => Recipe::Catalog(native::catalog::Kind::Triggers),
            SnapshotDataset::Partitions => Recipe::Catalog(native::catalog::Kind::Partitions),
            SnapshotDataset::Statistics => Recipe::Catalog(native::catalog::Kind::Statistics),
            SnapshotDataset::Sequences => Recipe::Catalog(native::catalog::Kind::Sequences),
            _ => return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))),
        },
        _ => return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))),
    };
    crate::probe::database_of(&connector)?;
    let reader = MysqlSnapshotReader {
        io: ReadSession::new(connector.clone(), request.budget.clone(), Scope::Server),
        connector,
        declared: if matches!(recipe, Recipe::Operational(native::operational::Kind::Status)) {
            native::operational::ALL.iter().copied().map(Recipe::Operational).collect()
        } else if matches!(recipe, Recipe::Catalog(native::catalog::Kind::Statistics)) {
            vec![recipe, Recipe::Catalog(native::catalog::Kind::Histograms)]
        } else {
            vec![recipe]
        },
        pending: VecDeque::new(),
        current: None,
        reading: false,
        unavailable: BTreeMap::new(),
        not_applicable: BTreeSet::new(),
        metadata: None,
        layout: None,
        budget: request.budget.clone(),
    };
    Ok(Box::new(SnapshotSource::new(reader, request)?))
}
struct MysqlSnapshotReader {
    io: ReadSession,
    connector: Arc<Connector>,
    declared: Vec<Recipe>,
    pending: VecDeque<Recipe>,
    current: Option<Recipe>,
    reading: bool,
    unavailable: BTreeMap<String, ErrorCode>,
    not_applicable: BTreeSet<String>,
    metadata: Option<Metadata>,
    layout: Option<native::statements::Layout>,
    budget: CollectionBudget,
}
#[async_trait::async_trait]
impl SnapshotReader for MysqlSnapshotReader {
    type Row = Rec;
    async fn describe(&mut self, _: &CollectionRequest) -> Result<CollectionDescription> {
        let meta = metadata::read(&mut self.io, &self.connector).await?;
        if self.declared.iter().any(|recipe| matches!(recipe, Recipe::Statements | Recipe::Definitions)) {
            if !meta.performance_schema {
                return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
            }
            let layout = native::statements::Layout::read(&mut self.io).await?;
            layout.require()?;
            self.layout = Some(layout);
        }
        let description = CollectionDescription {
            target: meta.database.clone().into(),
            source: format!("mysql-{}", self.declared[0].source()),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PagedSnapshot(PagedSnapshotDescription {
                dataset: self.declared[0].dataset(),
                observed_from: meta.at.clone(),
                sources: self.declared.iter().map(|recipe| recipe.source().into()).collect(),
            }),
        };
        self.metadata = Some(meta);
        Ok(description)
    }
    async fn begin(&mut self, _: &CollectionRequest) -> Result<()> {
        let meta = self.metadata.as_ref().ok_or_else(native::invalid)?;
        for &recipe in &self.declared {
            if matches!(recipe, Recipe::Catalog(native::catalog::Kind::Histograms))
                && meta.flavor == crate::connect::Flavor::Mysql
                && !culprit_core::version_at_least(&meta.version, [8, 0, 0])
            {
                self.not_applicable.insert(recipe.source().into());
            } else if matches!(recipe, Recipe::Operational(kind) if kind.requires_instrumentation() && !meta.performance_schema)
            {
                self.unavailable.insert(recipe.source().into(), ErrorCode::Unsupported);
            } else {
                self.pending.push_back(recipe);
            }
        }
        Ok(())
    }
    async fn next_batch(&mut self) -> Result<ReadBatch<Rec>> {
        loop {
            if !self.reading {
                let recipe = self.pending.pop_front().ok_or_else(native::invalid)?;
                let meta = self.metadata.as_ref().ok_or_else(native::invalid)?;
                let scoped = matches!(recipe, Recipe::Statements | Recipe::Definitions | Recipe::Catalog(_))
                    || matches!(recipe, Recipe::Operational(kind) if kind.database_scoped());
                let parameters = if scoped { native::statements::parameters(&meta.database_name) } else { vec![] };
                self.io
                    .begin_stream(
                        &recipe.query(meta, self.layout.as_ref(), &self.budget)?,
                        parameters,
                        self.budget.max_snapshot_rows as usize + 1,
                    )
                    .await?;
                self.current = Some(recipe);
                self.reading = true;
            }
            let recipe = self.current.ok_or_else(native::invalid)?;
            let mut batch = match self.io.next_batch().await {
                Ok(batch) => batch,
                Err(error)
                    if matches!(
                        recipe,
                        Recipe::Operational(_) | Recipe::Catalog(native::catalog::Kind::Histograms)
                    ) && error.code == ErrorCode::PermissionDenied =>
                {
                    self.unavailable.insert(recipe.source().into(), error.code);
                    self.reading = false;
                    if self.pending.is_empty() {
                        return Ok(ReadBatch { rows: vec![], complete: true });
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            if batch.complete {
                self.reading = false;
                if !self.pending.is_empty() {
                    if batch.rows.is_empty() {
                        continue;
                    }
                    batch.complete = false;
                }
            }
            return Ok(batch);
        }
    }
    fn decode(&self, row: &Rec, source: &CollectionDescription) -> Result<SnapshotRecord> {
        let recipe = self.current.ok_or_else(native::invalid)?;
        let meta = self.metadata.as_ref().ok_or_else(native::invalid)?;
        Ok(SnapshotRecord {
            source: recipe.source().into(),
            record: recipe.decode(row, meta, self.layout.as_ref(), source, &self.budget)?,
            complete_scope: !matches!(recipe, Recipe::Catalog(_)) || meta.database_wide,
        })
    }
    async fn recheck(&mut self, _: &CollectionDescription) -> Result<SnapshotRecheck> {
        let after = metadata::read(&mut self.io, &self.connector).await?;
        let before = self.metadata.as_ref().ok_or_else(native::invalid)?;
        let layout_after =
            if self.layout.is_some() { Some(native::statements::Layout::read(&mut self.io).await?) } else { None };
        let compatible_layout = self.layout.as_ref().zip(layout_after.as_ref()).is_none_or(|(a, b)| {
            a.columns == b.columns && a.cpu_enabled == b.cpu_enabled && a.digest_enabled == b.digest_enabled
        });
        let complete = self.layout.as_ref().is_none_or(|layout| layout.overflow_calls == 0)
            && layout_after.as_ref().is_none_or(|layout| layout.overflow_calls == 0);
        Ok(SnapshotRecheck {
            unchanged: before.same_source(&after) && compatible_layout,
            observed_until: after.at,
            scopes: self
                .declared
                .iter()
                .map(|recipe| match self.unavailable.get(recipe.source()) {
                    Some(&error_code) => SnapshotScope::Unavailable { source: recipe.source().into(), error_code },
                    None if self.not_applicable.contains(recipe.source()) => {
                        SnapshotScope::NotApplicable { source: recipe.source().into() }
                    }
                    None => SnapshotScope::Available {
                        source: recipe.source().into(),
                        complete: complete && (!matches!(recipe, Recipe::Catalog(_)) || before.database_wide),
                        error_code: None,
                    },
                })
                .collect(),
        })
    }
    fn query_count(&self) -> u32 {
        self.io.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}
