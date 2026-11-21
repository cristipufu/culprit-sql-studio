//! One source-owned catalog drives both declarations and native construction. A declaration is not live
//! availability or a production cost qualification, and no constructor performs native work.
use super::*;
use culprit_core::telemetry::*;

type OpenSource = fn(Arc<Pool>, &CollectionRequest, String, u16) -> Result<Box<dyn TelemetrySource>>;
type BoundBudget = fn(&CollectionBudget) -> CollectionBudget;

struct Registration {
    id: &'static str,
    version: u32,
    purpose: RecipePurpose,
    selector: RecipeSelector,
    subjects: &'static [RecipeSubject],
    editions: &'static [i64],
    budget: BoundBudget,
    open: OpenSource,
}
impl Registration {
    const fn database(id: &'static str, selector: RecipeSelector, open: OpenSource) -> Self {
        Self {
            id,
            version: 1,
            purpose: RecipePurpose::Investigation,
            selector,
            subjects: &[RecipeSubject::Database],
            editions: &[],
            budget: Clone::clone,
            open,
        }
    }
    const fn baseline(mut self) -> Self {
        self.purpose = RecipePurpose::Baseline;
        self
    }
    const fn version(mut self, version: u32) -> Self {
        self.version = version;
        self
    }
    const fn shared(mut self, subjects: &'static [RecipeSubject]) -> Self {
        self.subjects = subjects;
        self
    }
    const fn bounded(mut self, budget: BoundBudget) -> Self {
        self.budget = budget;
        self
    }
    const fn on_editions(mut self, editions: &'static [i64]) -> Self {
        self.editions = editions;
        self
    }
    fn descriptor(&self) -> RecipeDescriptor {
        RecipeDescriptor {
            id: self.id.into(),
            version: self.version,
            purpose: self.purpose,
            selector: self.selector,
            subjects: self.subjects.to_vec(),
            budget: self.native_budget(),
            cost: RecipeCostQualification::Unqualified,
        }
    }
    fn native_budget(&self) -> CollectionBudget {
        (self.budget)(&CollectionBudget::upper_limits())
    }
    fn accepts(&self, request: &CollectionRequest) -> bool {
        self.selector.matches(request) && self.subjects.iter().any(|subject| subject.accepts(&request.subject))
    }
    fn bounded_request(&self, request: &CollectionRequest) -> CollectionRequest {
        let mut request = request.clone();
        request.budget = request.budget.intersection(&self.native_budget());
        request
    }
}

macro_rules! streamed {
    ($id:literal, $dataset:ident, $recipe:path) => {
        Registration::database(
            $id,
            RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::$dataset)),
            |pool, request, host, port| {
                Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, $recipe)?))
            },
        )
    };
}
macro_rules! state {
    ($id:literal, $dataset:ident) => {
        Registration::database(
            $id,
            RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::$dataset)),
            |pool, request, host, port| Ok(Box::new(state::StateSource::new(pool, request, host, port)?)),
        )
    };
}

const REGISTRATIONS: &[Registration] = &[
    state!("mssql.database-performance-counters", DatabasePerformanceCounters)
        .on_editions(&[2, 3, 4])
        .bounded(state::local_point_budget)
        .version(2),
    state!("mssql.suspect-pages", SuspectPages).on_editions(&[2, 3, 4]).bounded(state::local_point_budget).version(2),
    Registration::database(
        "mssql.agent-jobs",
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::Jobs)),
        crate::jobs::source,
    )
    .shared(&[RecipeSubject::Database, RecipeSubject::Shared(SharedResourceKind::Instance)])
    .bounded(crate::jobs::collection_budget)
    .version(2),
    streamed!(
        "mssql.hyperscale-log-components",
        HyperscaleLogComponents,
        hyperscale_log_components::HyperscaleLogComponents
    )
    .bounded(hyperscale_log_components::bounded_budget)
    .on_editions(&[5])
    .version(2),
    Registration::database(
        "mssql.query-history",
        RecipeSelector::Collection(TelemetryCollection::QueryHistory),
        |pool, request, host, port| Ok(Box::new(Source::new(pool, request, host, port)?)),
    )
    .baseline()
    .version(4),
    Registration::database(
        "mssql.resource-history",
        RecipeSelector::Collection(TelemetryCollection::ResourceHistory),
        |pool, request, host, port| Ok(Box::new(resources::ResourceSource::new(pool, request, host, port)?)),
    )
    .baseline()
    .version(3),
    Registration::database(
        "mssql.pool-resource-history",
        RecipeSelector::Collection(TelemetryCollection::ResourceHistory),
        |pool, request, _, _| resources::pool_source(pool, request),
    )
    .shared(&[RecipeSubject::Shared(SharedResourceKind::Pool)])
    .baseline()
    .version(2),
    Registration::database(
        "mssql.deadlock-history",
        RecipeSelector::Collection(TelemetryCollection::DeadlockHistory),
        |pool, request, host, port| Ok(Box::new(events::EventSource::new(pool, request, host, port)?)),
    )
    .baseline()
    .version(2),
    Registration::database(
        "mssql.system-health-deadlocks",
        RecipeSelector::Collection(TelemetryCollection::DeadlockHistory),
        |pool, request, host, port| Ok(Box::new(events::EventSource::new(pool, request, host, port)?)),
    )
    .shared(&[RecipeSubject::Shared(SharedResourceKind::Instance)])
    .on_editions(crate::instance::SUPPORTED_EDITIONS)
    .bounded(events::shared_budget)
    .version(2),
    Registration::database(
        "mssql.activity",
        RecipeSelector::Collection(TelemetryCollection::Activity),
        |pool, request, host, port| Ok(Box::new(activity::ActivitySource::new(pool, request, host, port)?)),
    )
    .bounded(activity::bounded_budget)
    .baseline()
    .version(5),
    Registration::database(
        "mssql.page-wait-targets",
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::PageWaitTargets)),
        |pool, request, host, port| Ok(Box::new(page_waits::PageWaitSource::new(pool, request, host, port)?)),
    )
    .bounded(page_waits::bounded_budget)
    .version(3),
    Registration::database("mssql.page-wait-tasks", RecipeSelector::PageWaitTasks, |pool, request, host, port| {
        Ok(Box::new(page_waits::PageWaitSource::new(pool, request, host, port)?))
    })
    .bounded(page_waits::bounded_budget)
    .version(3),
    Registration::database(
        "mssql.shared-tempdb-page-wait-targets",
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::PageWaitTargets)),
        |pool, request, host, port| Ok(Box::new(page_waits::PageWaitSource::new(pool, request, host, port)?)),
    )
    .shared(&[RecipeSubject::Shared(SharedResourceKind::Instance)])
    .bounded(page_waits::bounded_budget)
    .version(4),
    Registration::database("mssql.focused-page-wait", RecipeSelector::FocusedPageWait, |pool, request, host, port| {
        Ok(Box::new(page_waits::PageWaitSource::new(pool, request, host, port)?))
    })
    .bounded(page_waits::focused_budget)
    .version(3),
    Registration::database(
        "mssql.focused-page-task-holders",
        RecipeSelector::FocusedPageTaskHolders,
        |pool, request, host, port| Ok(Box::new(page_waits::PageWaitSource::new(pool, request, host, port)?)),
    )
    .bounded(page_waits::holder_budget)
    .version(3),
    Registration::database(
        "mssql.focused-page-wait-writers",
        RecipeSelector::FocusedPageWaitWriters,
        |pool, request, host, port| Ok(Box::new(page_waits::PageWaitSource::new(pool, request, host, port)?)),
    )
    .bounded(page_waits::writer_budget)
    .version(4),
    Registration::database(
        "mssql.focused-shared-tempdb-page-wait",
        RecipeSelector::FocusedPageWait,
        |pool, request, host, port| Ok(Box::new(page_waits::PageWaitSource::new(pool, request, host, port)?)),
    )
    .shared(&[RecipeSubject::Shared(SharedResourceKind::Instance)])
    .bounded(page_waits::focused_budget)
    .version(4),
    Registration::database(
        "mssql.focused-compiler-admission",
        RecipeSelector::FocusedCompilationAdmission,
        |pool, request, host, port| {
            Ok(Box::new(compiler_admission::CompilerAdmissionSource::new(pool, request, host, port)?))
        },
    )
    .bounded(compiler_admission::focused_budget)
    .version(3),
    Registration::database(
        "mssql.focused-shared-compiler-admission",
        RecipeSelector::FocusedCompilationAdmission,
        |pool, request, host, port| {
            Ok(Box::new(compiler_admission::CompilerAdmissionSource::new(pool, request, host, port)?))
        },
    )
    .shared(&[RecipeSubject::Shared(SharedResourceKind::Instance)])
    .bounded(compiler_admission::focused_budget)
    .version(3),
    Registration::database(
        "mssql.current-request-memory",
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::CurrentRequestMemory)),
        |pool, request, host, port| Ok(Box::new(request_memory::MemorySource::new(pool, request, host, port)?)),
    )
    .bounded(request_memory::bounded_budget)
    .version(5),
    Registration::database(
        "mssql.focused-request-memory",
        RecipeSelector::FocusedRequestMemory,
        |pool, request, host, port| Ok(Box::new(request_memory::MemorySource::new(pool, request, host, port)?)),
    )
    .bounded(request_memory::bounded_budget)
    .version(5),
    Registration::database(
        "mssql.cache-entry-memory",
        RecipeSelector::CacheEntryMemory,
        |pool, request, host, port| {
            Ok(Box::new(cache_entry_memory::CacheEntryMemorySource::new(pool, request, host, port)?))
        },
    )
    .bounded(cache_entry_memory::bounded_budget)
    .version(2),
    Registration::database(
        "mssql.task-space",
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::TaskSpace)),
        |pool, request, host, port| Ok(Box::new(task_space::TaskSpaceSource::new(pool, request, host, port)?)),
    )
    .bounded(task_space::bounded_budget)
    .version(4),
    Registration::database(
        "mssql.cleanup",
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::Cleanup)),
        |pool, request, host, port| Ok(Box::new(cleanup::CleanupSource::new(pool, request, host, port)?)),
    )
    .bounded(cleanup::bounded_budget)
    .on_editions(crate::instance::SUPPORTED_EDITIONS)
    .version(2),
    Registration::database("mssql.table-memory", RecipeSelector::TableMemory, |pool, request, host, port| {
        Ok(Box::new(table_memory::TableMemorySource::new(pool, request, host, port)?))
    })
    .bounded(table_memory::bounded_budget),
    Registration::database(
        "mssql.columnstore-rowgroups",
        RecipeSelector::ColumnstoreRowgroups,
        |pool, request, host, port| {
            Ok(Box::new(columnstore_rowgroups::RowgroupSource::new(pool, request, host, port)?))
        },
    )
    .bounded(columnstore_rowgroups::bounded_budget)
    .version(2),
    Registration::database(
        "mssql.full-text-populations",
        RecipeSelector::FullTextPopulations,
        |pool, request, host, port| {
            Ok(Box::new(full_text_populations::FullTextPopulationSource::new(pool, request, host, port)?))
        },
    )
    .bounded(full_text_populations::bounded_budget),
    Registration::database(
        "mssql.cdc",
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::Cdc)),
        |pool, request, host, port| Ok(Box::new(cdc::CdcSource::new(pool, request, host, port)?)),
    )
    .bounded(cdc::bounded_budget)
    .version(2),
    Registration::database(
        "mssql.backup-history",
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::BackupHistory)),
        |pool, request, host, port| Ok(Box::new(backup_history::BackupHistorySource::new(pool, request, host, port)?)),
    )
    .bounded(backup_history::bounded_budget)
    .on_editions(&[2, 3, 4, 5, 8])
    .version(2),
    state!("mssql.operational", Operational).baseline().version(8),
    state!("mssql.shared-operational", Operational)
        .shared(&[RecipeSubject::Shared(SharedResourceKind::Instance)])
        .baseline()
        .version(9),
    state!("mssql.memory-failures", MemoryFailures)
        .shared(&[RecipeSubject::Shared(SharedResourceKind::Instance), RecipeSubject::Shared(SharedResourceKind::Pool)])
        .bounded(state::memory_failure_budget)
        .on_editions(&[crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE, crate::probe::ENGINE_EDITION_MANAGED_INSTANCE])
        .baseline()
        .version(2),
    state!("mssql.plan-cache-summary", PlanCacheSummary).baseline().version(2),
    state!("mssql.missing-indexes", MissingIndexes).version(2),
    state!("mssql.tuning", Tuning).bounded(state::tuning_budget).version(3),
    state!("mssql.wait-counters", WaitCounters).baseline().version(2),
    Registration::database(
        "mssql.replication",
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::Replication)),
        |pool, request, host, port| Ok(Box::new(replication::ReplicationSource::new(pool, request, host, port)?)),
    )
    .bounded(replication::bounded_budget)
    .baseline()
    .version(3),
    state!("mssql.settings", Settings).bounded(state::settings_budget).baseline().version(5),
    streamed!("mssql.statement-grants", StatementGrants, grants::Grants).version(2),
    streamed!("mssql.index-operations", IndexOperations, index_operations::IndexOperations).version(2),
    streamed!("mssql.tables", Tables, tables::Tables).version(5),
    streamed!("mssql.foreign-keys", ForeignKeys, foreign_keys::ForeignKeys).version(2),
    streamed!("mssql.columns", Columns, columns::Columns).version(2),
    streamed!("mssql.statistics", Statistics, statistics::Statistics).version(2),
    streamed!("mssql.indexes", Indexes, indexes::Indexes).version(3),
    streamed!("mssql.check-constraints", CheckConstraints, check_constraints::CheckConstraints).version(2),
    streamed!("mssql.query-compilation", QueryCompilation, compilation::QueryCompilation).version(4),
    streamed!("mssql.cached-runtime", CachedRuntime, cached_runtime::CachedRuntime).version(3),
    streamed!("mssql.objects", Objects, objects::Objects).version(2),
    streamed!("mssql.triggers", Triggers, triggers::Triggers).version(2),
    streamed!("mssql.partitions", Partitions, partitions::Partitions).version(2),
    streamed!("mssql.cached-definitions", CachedDefinitions, cached_definitions::CachedDefinitions).version(6),
    streamed!("mssql.retained-plans", RetainedPlans, retained_definitions::RetainedPlans)
        .version(7)
        .bounded(retained_definitions::bounded_budget),
];

pub(super) fn declarations() -> Vec<RecipeDescriptor> {
    REGISTRATIONS.iter().map(Registration::descriptor).collect()
}

/// Discovery has already observed the native platform. Derive its routes from the same registrations without
/// equating this platform filter with live permissions, retained evidence or production work qualification.
pub(super) fn collections_for(resource: &SharedResourceIdentity, edition: i64) -> Vec<TelemetryCollection> {
    let subject = CollectionSubject::Shared { resource: resource.clone() };
    let mut collections = Vec::new();
    for registration in REGISTRATIONS {
        if registration.subjects.iter().any(|kind| kind.accepts(&subject))
            && (registration.editions.is_empty() || registration.editions.contains(&edition))
            && let RecipeSelector::Collection(collection) = registration.selector
            && !collections.contains(&collection)
        {
            collections.push(collection);
        }
    }
    collections
}

pub(super) fn open(
    pool: Arc<Pool>,
    request: &CollectionRequest,
    host: String,
    port: u16,
) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    subject::is_shared(&request.subject)?;
    let recipe = REGISTRATIONS.iter().find(|recipe| recipe.accepts(request)).ok_or_else(|| {
        culprit_core::Error::new(culprit_core::ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))
    })?;
    let bounded = recipe.bounded_request(request);
    bounded.budget.validate()?;
    (recipe.open)(pool, &bounded, host, port)
}

#[cfg(test)]
mod tests;
