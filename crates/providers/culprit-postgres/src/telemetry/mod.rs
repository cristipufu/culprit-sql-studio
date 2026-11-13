//! Provider-owned recipes over the common capture contract and bounded native reader.
mod activity;
#[cfg(test)]
mod native_tests;
use crate::{
    connect::Connector,
    native::{
        self,
        metadata::{self, Metadata},
    },
    read::{NativeRead, ReadSession},
    rows::Row,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
use culprit_read::{ReadBatch, SnapshotReader, SnapshotRecheck, SnapshotRecord, SnapshotScope, SnapshotSource};
#[derive(Clone, Copy)]
enum Recipe {
    Statements,
    Definitions,
    Operational,
    InstanceOperational,
    Settings,
    Tables,
    Indexes,
    Replication,
    Sequences,
    Cleanup,
    Catalog(native::catalog::Kind),
}
impl Recipe {
    fn dataset(self) -> SnapshotDataset {
        match self {
            Self::Statements => SnapshotDataset::StatementCounters,
            Self::Definitions => SnapshotDataset::StatementDefinitions,
            Self::Operational | Self::InstanceOperational => SnapshotDataset::Operational,
            Self::Settings => SnapshotDataset::Settings,
            Self::Tables => SnapshotDataset::Tables,
            Self::Indexes => SnapshotDataset::Indexes,
            Self::Replication => SnapshotDataset::Replication,
            Self::Sequences => SnapshotDataset::Sequences,
            Self::Cleanup => SnapshotDataset::Cleanup,
            Self::Catalog(kind) => kind.dataset(),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Statements => "postgres-statement-counters",
            Self::Definitions => "postgres-statement-definitions",
            Self::Operational => "postgres-operational-state",
            Self::InstanceOperational => "postgres-instance-state",
            Self::Settings => "postgres-settings",
            Self::Tables => "postgres-tables",
            Self::Indexes => "postgres-indexes",
            Self::Replication => "postgres-replication",
            Self::Sequences => "postgres-sequences",
            Self::Cleanup => "postgres-cleanup",
            Self::Catalog(kind) => kind.object_type(),
        }
    }
    fn sources(self, meta: &Metadata) -> Vec<String> {
        match self {
            Self::Statements | Self::Definitions => vec!["pg_stat_statements".into()],
            Self::Operational => native::operational::selected_sources(meta, native::operational::Selection::Database)
                .into_iter()
                .map(str::to_owned)
                .collect(),
            Self::InstanceOperational => {
                native::operational::selected_sources(meta, native::operational::Selection::Instance)
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            }
            Self::Settings => vec!["pg_settings".into()],
            Self::Tables => vec!["pg_stat_user_tables".into()],
            Self::Indexes => vec!["pg_index".into()],
            Self::Replication => native::replication::sources(meta.standby),
            Self::Sequences => vec!["pg_sequences".into()],
            Self::Cleanup => native::cleanup::sources(),
            Self::Catalog(kind) => vec![kind.source().into()],
        }
    }
    fn query(
        self,
        meta: &Metadata,
        budget: &CollectionBudget,
        memory_access: native::memory::Access,
    ) -> Result<String> {
        let limit = budget.max_snapshot_rows as usize + 1;
        match self {
            Self::Statements | Self::Definitions => {
                native::statements::query(meta, matches!(self, Self::Definitions), limit, budget.max_sql_bytes as usize)
            }
            Self::Operational => Ok(native::operational::selected_query(
                meta,
                limit,
                native::operational::Selection::Database,
                memory_access,
            )),
            Self::InstanceOperational => Ok(native::operational::selected_query(
                meta,
                limit,
                native::operational::Selection::Instance,
                memory_access,
            )),
            Self::Settings => Ok(native::settings::capture_query(limit)),
            Self::Catalog(kind) => Ok(native::catalog::query(kind, meta.version, false, limit)),
            Self::Sequences => Ok(native::sequences::query(Some(limit))),
            Self::Cleanup => Ok(native::cleanup::capture_query(limit)),
            Self::Replication => Ok(native::replication::query(meta.version, meta.standby, &meta.database_oid, limit)),
            Self::Indexes => Ok(native::indexes::query(meta.version, false, false, Some(limit))),
            Self::Tables => Ok(native::relations::table_query(
                meta.version,
                native::relations::SizeRead::CatalogEstimate,
                Some(limit),
            )),
        }
    }
    fn decode(
        self,
        row: &Row,
        meta: &Metadata,
        source: &CollectionDescription,
        budget: &CollectionBudget,
    ) -> Result<(String, TelemetryRecord)> {
        match self {
            Self::Statements => {
                let sample = native::statements::counters(row, meta)?;
                let record = OperationalObservation {
                    source: "pg_stat_statements".into(),
                    key: sample.key()?,
                    fact: OperationalFact::StatementCounters(Box::new(sample)),
                };
                Ok((record.source.clone(), TelemetryRecord::Operational(record)))
            }
            Self::Definitions => {
                let definition = native::statements::definition(row, meta, source, budget.max_sql_bytes as usize)?;
                let record = ObjectDefinitionRecord::new(
                    "pg_stat_statements".into(),
                    ObjectDefinitionFact::Statement(Box::new(definition)),
                )?;
                Ok((record.source.clone(), TelemetryRecord::ObjectDefinition(record)))
            }
            Self::Catalog(kind) => {
                let definition = native::catalog::decode(
                    kind,
                    row.clone(),
                    Some(&meta.database_oid),
                    budget.max_sql_bytes as usize,
                    culprit_core::Engine::Postgres,
                )?
                .definition;
                let source = kind.source().to_owned();
                let record = if matches!(kind, native::catalog::Kind::Statistics) {
                    let counters = NativeCounterSet {
                        identity: definition.identity.clone(),
                        scope: culprit_core::MeasurementScope::Database,
                        counters: vec![],
                        properties: std::collections::BTreeMap::from([(
                            "object_type".into(),
                            kind.object_type().into(),
                        )]),
                    };
                    let key = definition.key()?;
                    TelemetryRecord::Operational(OperationalObservation {
                        source: source.clone(),
                        key,
                        fact: OperationalFact::CatalogCounters(Box::new(NativeCatalogCounters {
                            catalog: definition,
                            counters,
                        })),
                    })
                } else {
                    TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
                        source.clone(),
                        ObjectDefinitionFact::NativeCatalog(Box::new(definition)),
                    )?)
                };
                Ok((source, record))
            }
            Self::Cleanup => {
                let holder = native::cleanup::decode(row, &meta.database_oid)?;
                let source = native::text(row, "native_source")?;
                Ok((
                    source.clone(),
                    TelemetryRecord::Operational(OperationalObservation {
                        source,
                        key: holder.catalog.key()?,
                        fact: OperationalFact::CatalogCounters(Box::new(holder)),
                    }),
                ))
            }
            Self::Sequences => {
                let sequence = native::sequences::decode(row.clone())?;
                let source = "pg_sequences".to_owned();
                Ok((
                    source.clone(),
                    TelemetryRecord::Operational(OperationalObservation {
                        source,
                        key: sequence.catalog.key()?,
                        fact: OperationalFact::CatalogCounters(Box::new(sequence)),
                    }),
                ))
            }
            Self::Replication => {
                let row = native::replication::decode(row, &meta.database_oid)?;
                Ok((row.source.clone(), TelemetryRecord::Replication(row)))
            }
            Self::Indexes => {
                let index = native::indexes::decode(row.clone(), Some(&meta.database_oid))?.native;
                let record = OperationalObservation {
                    source: "pg_index".into(),
                    key: index.catalog.key()?,
                    fact: OperationalFact::CatalogCounters(Box::new(index)),
                };
                Ok((record.source.clone(), TelemetryRecord::Operational(record)))
            }
            Self::Tables => {
                let set = native::relations::table(row, &meta.database_oid)?;
                let record = OperationalObservation {
                    source: "pg_stat_user_tables".into(),
                    key: set.key()?,
                    fact: OperationalFact::CounterSet(Box::new(set)),
                };
                Ok((record.source.clone(), TelemetryRecord::Operational(record)))
            }
            Self::Settings => Ok(("pg_settings".into(), TelemetryRecord::Setting(native::settings::decode(row)?))),
            Self::Operational | Self::InstanceOperational => {
                let selection = if matches!(self, Self::InstanceOperational) {
                    native::operational::Selection::Instance
                } else {
                    native::operational::Selection::Database
                };
                let record = native::operational::decode(row, meta, selection)?;
                let TelemetryRecord::Operational(observation) = &record else { unreachable!() };
                Ok((observation.source.clone(), record))
            }
        }
    }
}
pub(crate) fn source(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    if let CollectionSubject::Shared { resource } = &request.subject {
        if resource.engine != culprit_core::Engine::Postgres
            || resource.kind != SharedResourceKind::Instance
            || request.collection() != TelemetryCollection::Snapshot(SnapshotDataset::Operational)
        {
            return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
        }
        return snapshot_source(connector, request, Recipe::InstanceOperational);
    }
    if request.collection() == TelemetryCollection::Activity {
        return activity::source(connector, request);
    }
    let recipe = match request.collection() {
        TelemetryCollection::Snapshot(SnapshotDataset::StatementCounters) => Recipe::Statements,
        TelemetryCollection::Snapshot(SnapshotDataset::StatementDefinitions) => Recipe::Definitions,
        TelemetryCollection::Snapshot(SnapshotDataset::Operational) => Recipe::Operational,
        TelemetryCollection::Snapshot(SnapshotDataset::Settings) => Recipe::Settings,
        TelemetryCollection::Snapshot(SnapshotDataset::Tables) => Recipe::Tables,
        TelemetryCollection::Snapshot(SnapshotDataset::Indexes) => Recipe::Indexes,
        TelemetryCollection::Snapshot(SnapshotDataset::Replication) => Recipe::Replication,
        TelemetryCollection::Snapshot(SnapshotDataset::Sequences) => Recipe::Sequences,
        TelemetryCollection::Snapshot(SnapshotDataset::Cleanup) => Recipe::Cleanup,
        TelemetryCollection::Snapshot(SnapshotDataset::Columns) => Recipe::Catalog(native::catalog::Kind::Columns),
        TelemetryCollection::Snapshot(SnapshotDataset::ForeignKeys) => {
            Recipe::Catalog(native::catalog::Kind::ForeignKeys)
        }
        TelemetryCollection::Snapshot(SnapshotDataset::CheckConstraints) => {
            Recipe::Catalog(native::catalog::Kind::Checks)
        }
        TelemetryCollection::Snapshot(SnapshotDataset::Objects) => Recipe::Catalog(native::catalog::Kind::Objects),
        TelemetryCollection::Snapshot(SnapshotDataset::Triggers) => Recipe::Catalog(native::catalog::Kind::Triggers),
        TelemetryCollection::Snapshot(SnapshotDataset::Partitions) => {
            Recipe::Catalog(native::catalog::Kind::Partitions)
        }
        TelemetryCollection::Snapshot(SnapshotDataset::Statistics) => {
            Recipe::Catalog(native::catalog::Kind::Statistics)
        }
        _ => return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))),
    };
    snapshot_source(connector, request, recipe)
}

pub(crate) async fn resources(
    connector: Connector,
    request: &ResourceDiscoveryRequest,
) -> Result<Vec<CollectionResource>> {
    request.budget.validate()?;
    if request.kind != SharedResourceKind::Instance {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let host = connector.host.clone();
    let port = connector.port;
    let mut reader = ReadSession::new(connector, request.budget.clone());
    let result = async {
        let metadata = metadata::read(&mut reader, &host, port).await?;
        let instance = native::instance::read(&mut reader, metadata.version).await?;
        let resources = vec![CollectionResource {
            identity: instance.identity,
            observed_at: instance.observed_at,
            collections: vec![TelemetryCollection::Snapshot(SnapshotDataset::Operational)],
        }];
        request.validate_result(&resources)?;
        Ok(resources)
    }
    .await;
    if reader.close().await == SourceShutdown::Unconfirmed {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    result
}

fn snapshot_source(
    connector: Connector,
    request: &CollectionRequest,
    recipe: Recipe,
) -> Result<Box<dyn TelemetrySource>> {
    let reader = PostgresSnapshotReader {
        host: connector.host.clone(),
        port: connector.port,
        io: ReadSession::new(connector, request.budget.clone()),
        budget: request.budget.clone(),
        recipe,
        metadata: None,
        instance: None,
        memory_access: native::memory::Access::Unsupported,
    };
    Ok(Box::new(SnapshotSource::new(reader, request)?))
}
struct PostgresSnapshotReader {
    io: ReadSession,
    budget: CollectionBudget,
    recipe: Recipe,
    host: String,
    port: u16,
    metadata: Option<Metadata>,
    instance: Option<SharedResourceIdentity>,
    memory_access: native::memory::Access,
}
#[async_trait::async_trait]
impl SnapshotReader for PostgresSnapshotReader {
    type Row = Row;
    async fn describe(&mut self, request: &CollectionRequest) -> Result<CollectionDescription> {
        let meta = metadata::read(&mut self.io, &self.host, self.port).await?;
        let instance = if matches!(self.recipe, Recipe::InstanceOperational) {
            Some(native::instance::read(&mut self.io, meta.version).await?.identity)
        } else {
            None
        };
        let target = instance.clone().map(CollectionTarget::Shared).unwrap_or_else(|| meta.database.clone().into());
        if !request.subject.accepts(&target) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let memory_access = if matches!(self.recipe, Recipe::InstanceOperational) {
            native::memory::access(&mut self.io, meta.version).await?
        } else {
            native::memory::Access::Unsupported
        };
        // Validate native support and permissions before admitting the statistics statement.
        self.recipe.query(&meta, &request.budget, memory_access)?;
        if matches!(self.recipe, Recipe::Replication | Recipe::Cleanup) && !meta.full_visibility {
            return Err(Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source")));
        }
        if matches!(self.recipe, Recipe::Catalog(native::catalog::Kind::Statistics))
            && !native::catalog::statistics_visible(&mut self.io).await?
        {
            return Err(Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source")));
        }
        let description = CollectionDescription {
            target,
            source: self.recipe.name().into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PagedSnapshot(PagedSnapshotDescription {
                dataset: self.recipe.dataset(),
                observed_from: meta.at.clone(),
                sources: self.recipe.sources(&meta),
            }),
        };
        if !description.matches_request(request) {
            return Err(CollectionStop::SourceChanged.error());
        }
        self.instance = instance;
        self.memory_access = memory_access;
        self.metadata = Some(meta);
        Ok(description)
    }
    async fn begin(&mut self, request: &CollectionRequest) -> Result<()> {
        let meta = self.metadata.as_ref().ok_or_else(native::invalid)?;
        let query = self.recipe.query(meta, &request.budget, self.memory_access)?;
        self.io.begin_stream(&query, request.budget.max_snapshot_rows as usize + 1).await
    }
    async fn next_batch(&mut self) -> Result<ReadBatch<Row>> {
        self.io.next_batch().await
    }
    fn decode(&self, row: &Row, description: &CollectionDescription) -> Result<SnapshotRecord> {
        let meta = self.metadata.as_ref().ok_or_else(native::invalid)?;
        let (source, record) = self.recipe.decode(row, meta, description, &self.budget)?;
        Ok(SnapshotRecord { source, record, complete_scope: true })
    }
    async fn recheck(&mut self, _: &CollectionDescription) -> Result<SnapshotRecheck> {
        let after = metadata::read(&mut self.io, &self.host, self.port).await?;
        let before = self.metadata.as_ref().ok_or_else(native::invalid)?;
        let same_instance = match &self.instance {
            Some(before) => native::instance::read(&mut self.io, after.version).await?.identity == *before,
            None => true,
        };
        let complete_scope = if matches!(self.recipe, Recipe::Catalog(native::catalog::Kind::Statistics)) {
            native::catalog::statistics_visible(&mut self.io).await?
        } else {
            true
        };
        let memory_after = if matches!(self.recipe, Recipe::InstanceOperational) {
            native::memory::access(&mut self.io, after.version).await?
        } else {
            native::memory::Access::Unsupported
        };
        Ok(SnapshotRecheck {
            unchanged: before.same_source(&after) && same_instance,
            observed_until: after.at,
            scopes: self
                .recipe
                .sources(before)
                .into_iter()
                .map(|source| {
                    if source == native::memory::SOURCE_NAME {
                        return match self.memory_access {
                            native::memory::Access::Available => SnapshotScope::Available {
                                source,
                                complete: memory_after == native::memory::Access::Available,
                                error_code: (memory_after != native::memory::Access::Available)
                                    .then_some(ErrorCode::PermissionDenied),
                            },
                            native::memory::Access::Restricted => {
                                SnapshotScope::Unavailable { source, error_code: ErrorCode::PermissionDenied }
                            }
                            native::memory::Access::Unsupported => SnapshotScope::NotApplicable { source },
                        };
                    }
                    SnapshotScope::Available {
                        source,
                        complete: complete_scope,
                        error_code: (!complete_scope).then_some(ErrorCode::PermissionDenied),
                    }
                })
                .collect(),
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
