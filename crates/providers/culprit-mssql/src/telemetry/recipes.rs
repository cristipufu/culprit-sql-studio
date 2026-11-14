//! One source-owned catalog drives both declarations and native construction. A declaration is not live
//! availability or a production cost qualification, and no constructor performs native work.
use super::*;
use culprit_core::telemetry::*;

type OpenSource = fn(Arc<Pool>, &CollectionRequest, String, u16) -> Result<Box<dyn TelemetrySource>>;
type BoundBudget = fn(&CollectionBudget) -> CollectionBudget;

struct Registration {
    id: &'static str,
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
            version: 1,
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
    Registration::database(
        "mssql.query-history",
        RecipeSelector::Collection(TelemetryCollection::QueryHistory),
        |pool, request, host, port| Ok(Box::new(Source::new(pool, request, host, port)?)),
    )
    .baseline(),
    Registration::database(
        "mssql.resource-history",
        RecipeSelector::Collection(TelemetryCollection::ResourceHistory),
        |pool, request, host, port| Ok(Box::new(resources::ResourceSource::new(pool, request, host, port)?)),
    )
    .baseline(),
    Registration::database(
        "mssql.pool-resource-history",
        RecipeSelector::Collection(TelemetryCollection::ResourceHistory),
        |pool, request, _, _| resources::pool_source(pool, request),
    )
    .shared(&[RecipeSubject::Shared(SharedResourceKind::Pool)])
    .baseline(),
    Registration::database(
        "mssql.deadlock-history",
        RecipeSelector::Collection(TelemetryCollection::DeadlockHistory),
        |pool, request, host, port| Ok(Box::new(events::EventSource::new(pool, request, host, port)?)),
    )
    .baseline(),
    Registration::database(
        "mssql.activity",
        RecipeSelector::Collection(TelemetryCollection::Activity),
        |pool, request, host, port| Ok(Box::new(activity::ActivitySource::new(pool, request, host, port)?)),
    )
    .bounded(activity::bounded_budget)
    .baseline(),
    Registration::database(
        "mssql.page-wait-targets",
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::PageWaitTargets)),
        |pool, request, host, port| Ok(Box::new(page_waits::PageWaitSource::new(pool, request, host, port)?)),
    )
    .bounded(crate::page_waits::bounded_budget),
    Registration::database("mssql.focused-page-wait", RecipeSelector::FocusedPageWait, |pool, request, host, port| {
        Ok(Box::new(page_waits::PageWaitSource::new(pool, request, host, port)?))
    })
    .bounded(page_waits::focused_budget),
    Registration::database(
        "mssql.current-request-memory",
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::CurrentRequestMemory)),
        |pool, request, host, port| Ok(Box::new(request_memory::MemorySource::new(pool, request, host, port)?)),
    )
    .bounded(crate::request_memory::bounded_budget),
    Registration::database(
        "mssql.task-space",
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::TaskSpace)),
        |pool, request, host, port| Ok(Box::new(task_space::TaskSpaceSource::new(pool, request, host, port)?)),
    )
    .bounded(crate::task_space::bounded_budget),
    state!("mssql.operational", Operational).baseline(),
    state!("mssql.shared-operational", Operational)
        .shared(&[RecipeSubject::Shared(SharedResourceKind::Instance)])
        .baseline(),
    state!("mssql.memory-failures", MemoryFailures)
        .shared(&[RecipeSubject::Shared(SharedResourceKind::Instance), RecipeSubject::Shared(SharedResourceKind::Pool)])
        .bounded(state::memory_failure_budget)
        .on_editions(&[crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE, crate::probe::ENGINE_EDITION_MANAGED_INSTANCE])
        .baseline(),
    state!("mssql.plan-cache-summary", PlanCacheSummary).baseline(),
    state!("mssql.missing-indexes", MissingIndexes),
    state!("mssql.tuning", Tuning),
    state!("mssql.wait-counters", WaitCounters).baseline(),
    state!("mssql.replication", Replication).baseline(),
    state!("mssql.settings", Settings).baseline(),
    streamed!("mssql.statement-grants", StatementGrants, grants::Grants),
    streamed!("mssql.index-operations", IndexOperations, index_operations::IndexOperations),
    streamed!("mssql.tables", Tables, tables::Tables),
    streamed!("mssql.foreign-keys", ForeignKeys, foreign_keys::ForeignKeys),
    streamed!("mssql.columns", Columns, columns::Columns),
    streamed!("mssql.statistics", Statistics, statistics::Statistics),
    streamed!("mssql.indexes", Indexes, indexes::Indexes),
    streamed!("mssql.check-constraints", CheckConstraints, check_constraints::CheckConstraints),
    streamed!("mssql.query-compilation", QueryCompilation, compilation::QueryCompilation),
    streamed!("mssql.cached-runtime", CachedRuntime, cached_runtime::CachedRuntime),
    streamed!("mssql.objects", Objects, objects::Objects),
    streamed!("mssql.triggers", Triggers, triggers::Triggers),
    streamed!("mssql.partitions", Partitions, partitions::Partitions),
    streamed!("mssql.cached-definitions", CachedDefinitions, cached_definitions::CachedDefinitions),
    streamed!("mssql.retained-plans", RetainedPlans, retained_definitions::RetainedPlans),
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
