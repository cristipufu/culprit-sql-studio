//! One native registration catalog owns declarations, dispatch and shared-resource routes, without source I/O.
use super::*;

#[derive(Clone, Copy)]
enum Route {
    Activity,
    Snapshot(Recipe),
}
impl Route {
    fn collection(self) -> TelemetryCollection {
        match self {
            Self::Activity => TelemetryCollection::Activity,
            Self::Snapshot(recipe) => TelemetryCollection::Snapshot(recipe.dataset()),
        }
    }
    fn selector(self) -> RecipeSelector {
        RecipeSelector::Collection(self.collection())
    }
    fn bounded_budget(self, requested: &CollectionBudget) -> CollectionBudget {
        match self {
            Self::Activity => activity::bounded_budget(requested),
            Self::Snapshot(_) => requested.clone(),
        }
    }
    fn open(self, connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        match self {
            Self::Activity => activity::source(connector, request),
            Self::Snapshot(recipe) => snapshot_source(connector, request, recipe),
        }
    }
}

struct Registration {
    id: &'static str,
    purpose: RecipePurpose,
    subject: RecipeSubject,
    route: Route,
}
impl Registration {
    const fn snapshot(id: &'static str, recipe: Recipe) -> Self {
        Self {
            id,
            purpose: RecipePurpose::Investigation,
            subject: RecipeSubject::Database,
            route: Route::Snapshot(recipe),
        }
    }
    const fn baseline(mut self) -> Self {
        self.purpose = RecipePurpose::Baseline;
        self
    }
    const fn shared(mut self, kind: SharedResourceKind) -> Self {
        self.subject = RecipeSubject::Shared(kind);
        self
    }
    fn descriptor(&self) -> RecipeDescriptor {
        RecipeDescriptor {
            id: self.id.into(),
            version: 1,
            purpose: self.purpose,
            selector: self.route.selector(),
            subjects: vec![self.subject],
            budget: self.native_budget(),
            cost: RecipeCostQualification::Unqualified,
        }
    }
    fn accepts(&self, request: &CollectionRequest) -> bool {
        self.route.selector().matches(request) && self.subject.accepts(&request.subject)
    }
    fn native_budget(&self) -> CollectionBudget {
        self.route.bounded_budget(&CollectionBudget::upper_limits())
    }
    fn bounded_request(&self, request: &CollectionRequest) -> CollectionRequest {
        let mut request = request.clone();
        request.budget = request.budget.intersection(&self.native_budget());
        request
    }
}

const REGISTRATIONS: &[Registration] = &[
    Registration {
        id: "postgres.activity",
        purpose: RecipePurpose::Baseline,
        subject: RecipeSubject::Database,
        route: Route::Activity,
    },
    Registration::snapshot("postgres.statement-counters", Recipe::Statements).baseline(),
    Registration::snapshot("postgres.statement-definitions", Recipe::Definitions),
    Registration::snapshot("postgres.operational", Recipe::Operational).baseline(),
    Registration::snapshot("postgres.instance-operational", Recipe::InstanceOperational)
        .shared(SharedResourceKind::Instance)
        .baseline(),
    Registration::snapshot("postgres.settings", Recipe::Settings).baseline(),
    Registration::snapshot("postgres.tables", Recipe::Tables).baseline(),
    Registration::snapshot("postgres.indexes", Recipe::Indexes).baseline(),
    Registration::snapshot("postgres.replication", Recipe::Replication).baseline(),
    Registration::snapshot("postgres.sequences", Recipe::Sequences).baseline(),
    Registration::snapshot("postgres.cleanup", Recipe::Cleanup).baseline(),
    Registration::snapshot("postgres.columns", Recipe::Catalog(native::catalog::Kind::Columns)),
    Registration::snapshot("postgres.foreign-keys", Recipe::Catalog(native::catalog::Kind::ForeignKeys)),
    Registration::snapshot("postgres.check-constraints", Recipe::Catalog(native::catalog::Kind::Checks)),
    Registration::snapshot("postgres.objects", Recipe::Catalog(native::catalog::Kind::Objects)),
    Registration::snapshot("postgres.triggers", Recipe::Catalog(native::catalog::Kind::Triggers)),
    Registration::snapshot("postgres.partitions", Recipe::Catalog(native::catalog::Kind::Partitions)),
    Registration::snapshot("postgres.statistics", Recipe::Catalog(native::catalog::Kind::Statistics)),
];

pub(crate) fn declarations() -> Vec<RecipeDescriptor> {
    REGISTRATIONS.iter().map(Registration::descriptor).collect()
}

pub(super) fn collections_for(resource: &SharedResourceIdentity) -> Vec<TelemetryCollection> {
    if resource.engine != culprit_core::Engine::Postgres {
        return Vec::new();
    }
    let subject = CollectionSubject::Shared { resource: resource.clone() };
    REGISTRATIONS
        .iter()
        .filter(|registration| registration.subject.accepts(&subject))
        .map(|registration| registration.route.collection())
        .collect()
}

pub(crate) fn open(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    if matches!(&request.subject, CollectionSubject::Shared { resource } if resource.engine != culprit_core::Engine::Postgres)
    {
        return Err(unsupported());
    }
    let registration =
        REGISTRATIONS.iter().find(|registration| registration.accepts(request)).ok_or_else(unsupported)?;
    let bounded = registration.bounded_request(request);
    bounded.budget.validate()?;
    registration.route.open(connector, &bounded)
}

fn unsupported() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))
}

#[cfg(test)]
mod tests;
