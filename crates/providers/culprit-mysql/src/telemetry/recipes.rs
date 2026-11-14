//! One source-owned catalog selects existing native readers and declares their bounded construction envelopes.
use super::*;

#[derive(Clone, Copy)]
enum Acquisition {
    Activity,
    Deadlocks,
    RuntimeDistributions,
    NativeFailures,
    SessionMemory,
    Snapshot(Recipe),
}
impl Acquisition {
    fn collection(self) -> TelemetryCollection {
        match self {
            Self::Activity => TelemetryCollection::Activity,
            Self::Deadlocks => TelemetryCollection::DeadlockHistory,
            Self::RuntimeDistributions => TelemetryCollection::Snapshot(SnapshotDataset::RuntimeDistributions),
            Self::NativeFailures => TelemetryCollection::Snapshot(SnapshotDataset::NativeFailures),
            Self::SessionMemory => TelemetryCollection::Snapshot(SnapshotDataset::SessionMemory),
            Self::Snapshot(recipe) => TelemetryCollection::Snapshot(recipe.dataset()),
        }
    }
    fn subject(self) -> RecipeSubject {
        match self {
            Self::NativeFailures | Self::SessionMemory => RecipeSubject::Shared(SharedResourceKind::Instance),
            _ => RecipeSubject::Database,
        }
    }
    fn budget(self) -> CollectionBudget {
        let budget = CollectionBudget::upper_limits();
        match self {
            Self::Activity => activity::bounded_budget(&budget),
            Self::RuntimeDistributions => runtime_distributions::bounded_budget(&budget),
            Self::NativeFailures => crate::native_failures::bounded_budget(&budget),
            Self::SessionMemory => crate::session_memory::bounded_budget(&budget),
            Self::Deadlocks | Self::Snapshot(_) => budget,
        }
    }
    fn open(self, connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        match self {
            Self::Activity => activity::source(connector, request),
            Self::Deadlocks => events::source(connector, request),
            Self::RuntimeDistributions => runtime_distributions::source(connector, request),
            Self::NativeFailures => native_failures::source(connector, request),
            Self::SessionMemory => session_memory::source(connector, request),
            Self::Snapshot(recipe) => snapshot_source(connector, request, recipe),
        }
    }
    fn available_on_instance(self, platform: &native::instance::Platform) -> bool {
        match self {
            Self::NativeFailures => {
                native::error_log::availability(platform.flavor, &platform.version, platform.performance_schema)
                    == native::error_log::Availability::Available
            }
            Self::SessionMemory => {
                native::session_memory::availability(platform.flavor, &platform.version, platform.performance_schema)
                    == native::session_memory::Availability::Available
            }
            _ => false,
        }
    }
}

struct Registration {
    id: &'static str,
    purpose: RecipePurpose,
    acquisition: Acquisition,
}
impl Registration {
    const fn baseline(id: &'static str, acquisition: Acquisition) -> Self {
        Self { id, purpose: RecipePurpose::Baseline, acquisition }
    }
    const fn investigation(id: &'static str, acquisition: Acquisition) -> Self {
        Self { id, purpose: RecipePurpose::Investigation, acquisition }
    }
    fn descriptor(&self) -> RecipeDescriptor {
        RecipeDescriptor {
            id: self.id.into(),
            version: 1,
            purpose: self.purpose,
            selector: RecipeSelector::Collection(self.acquisition.collection()),
            subjects: vec![self.acquisition.subject()],
            budget: self.acquisition.budget(),
            cost: RecipeCostQualification::Unqualified,
        }
    }
    fn accepts(&self, request: &CollectionRequest) -> bool {
        RecipeSelector::Collection(self.acquisition.collection()).matches(request)
            && self.acquisition.subject().accepts(&request.subject)
    }
    fn bounded_request(&self, request: &CollectionRequest) -> CollectionRequest {
        let mut request = request.clone();
        request.budget = request.budget.intersection(&self.acquisition.budget());
        request
    }
}

const REGISTRATIONS: &[Registration] = &[
    Registration::baseline("mysql.activity", Acquisition::Activity),
    Registration::baseline("mysql.deadlock-history", Acquisition::Deadlocks),
    Registration::baseline("mysql.statement-counters", Acquisition::Snapshot(Recipe::Statements)),
    Registration::baseline(
        "mysql.operational",
        Acquisition::Snapshot(Recipe::Operational(native::operational::Kind::Status)),
    ),
    Registration::baseline("mysql.replication", Acquisition::Snapshot(Recipe::Replication)),
    Registration::baseline("mysql.settings", Acquisition::Snapshot(Recipe::Settings)),
    Registration::baseline("mysql.native-failures", Acquisition::NativeFailures),
    Registration::investigation("mysql.runtime-distributions", Acquisition::RuntimeDistributions),
    Registration::investigation("mysql.session-memory", Acquisition::SessionMemory),
    Registration::investigation("mysql.statement-definitions", Acquisition::Snapshot(Recipe::Definitions)),
    Registration::investigation(
        "mysql.cleanup",
        Acquisition::Snapshot(Recipe::Operational(native::operational::Kind::Transactions)),
    ),
    Registration::investigation("mysql.tables", Acquisition::Snapshot(Recipe::Catalog(native::catalog::Kind::Tables))),
    Registration::investigation(
        "mysql.indexes",
        Acquisition::Snapshot(Recipe::Catalog(native::catalog::Kind::Indexes)),
    ),
    Registration::investigation(
        "mysql.columns",
        Acquisition::Snapshot(Recipe::Catalog(native::catalog::Kind::Columns)),
    ),
    Registration::investigation(
        "mysql.foreign-keys",
        Acquisition::Snapshot(Recipe::Catalog(native::catalog::Kind::ForeignKeys)),
    ),
    Registration::investigation(
        "mysql.check-constraints",
        Acquisition::Snapshot(Recipe::Catalog(native::catalog::Kind::Checks)),
    ),
    Registration::investigation(
        "mysql.objects",
        Acquisition::Snapshot(Recipe::Catalog(native::catalog::Kind::Objects)),
    ),
    Registration::investigation(
        "mysql.triggers",
        Acquisition::Snapshot(Recipe::Catalog(native::catalog::Kind::Triggers)),
    ),
    Registration::investigation(
        "mysql.partitions",
        Acquisition::Snapshot(Recipe::Catalog(native::catalog::Kind::Partitions)),
    ),
    Registration::investigation(
        "mysql.statistics",
        Acquisition::Snapshot(Recipe::Catalog(native::catalog::Kind::Statistics)),
    ),
    Registration::investigation(
        "mysql.sequences",
        Acquisition::Snapshot(Recipe::Catalog(native::catalog::Kind::Sequences)),
    ),
];

pub(super) fn declarations() -> Vec<RecipeDescriptor> {
    REGISTRATIONS.iter().map(Registration::descriptor).collect()
}
pub(super) fn open(connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    let registration = REGISTRATIONS
        .iter()
        .find(|registration| registration.accepts(request))
        .ok_or_else(|| Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")))?;
    let bounded = registration.bounded_request(request);
    bounded.budget.validate()?;
    registration.acquisition.open(connector, &bounded)
}

pub(super) fn instance_collections(platform: &native::instance::Platform) -> Vec<TelemetryCollection> {
    REGISTRATIONS
        .iter()
        .filter(|registration| registration.acquisition.available_on_instance(platform))
        .map(|registration| registration.acquisition.collection())
        .collect()
}

#[cfg(test)]
mod tests;
