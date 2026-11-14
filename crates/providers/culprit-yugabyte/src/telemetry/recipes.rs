//! Native YSQL declarations and dispatch use the same routes; PostgreSQL wire sharing grants no extra sources.
use super::*;

#[derive(Clone, Copy)]
enum Route {
    Activity,
    Operational,
    Snapshot(Recipe),
}
impl Route {
    fn collection(self) -> TelemetryCollection {
        match self {
            Self::Activity => TelemetryCollection::Activity,
            Self::Operational => TelemetryCollection::Snapshot(SnapshotDataset::Operational),
            Self::Snapshot(recipe) => TelemetryCollection::Snapshot(recipe.dataset()),
        }
    }
    fn selector(self) -> RecipeSelector {
        RecipeSelector::Collection(self.collection())
    }
    fn bounded_budget(self, requested: &CollectionBudget) -> CollectionBudget {
        match self {
            Self::Activity => activity::bounded_budget(requested),
            Self::Operational | Self::Snapshot(_) => requested.clone(),
        }
    }
    fn open(self, connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        match self {
            Self::Activity => activity::source(connector, request),
            Self::Operational => operational::source(connector, request),
            Self::Snapshot(recipe) => snapshot_source(connector, request, recipe),
        }
    }
}

struct Registration {
    id: &'static str,
    purpose: RecipePurpose,
    route: Route,
}
impl Registration {
    const fn snapshot(id: &'static str, recipe: Recipe) -> Self {
        Self { id, purpose: RecipePurpose::Investigation, route: Route::Snapshot(recipe) }
    }
    const fn baseline(mut self) -> Self {
        self.purpose = RecipePurpose::Baseline;
        self
    }
    fn descriptor(&self) -> RecipeDescriptor {
        RecipeDescriptor {
            id: self.id.into(),
            version: 1,
            purpose: self.purpose,
            selector: self.route.selector(),
            subjects: vec![RecipeSubject::Database],
            budget: self.native_budget(),
            cost: RecipeCostQualification::Unqualified,
        }
    }
    fn native_budget(&self) -> CollectionBudget {
        self.route.bounded_budget(&CollectionBudget::upper_limits())
    }
    fn accepts(&self, request: &CollectionRequest) -> bool {
        self.route.selector().matches(request) && RecipeSubject::Database.accepts(&request.subject)
    }
    fn bounded_request(&self, request: &CollectionRequest) -> CollectionRequest {
        let mut request = request.clone();
        request.budget = request.budget.intersection(&self.native_budget());
        request
    }
}

const REGISTRATIONS: &[Registration] = &[
    Registration { id: "yugabyte.activity", purpose: RecipePurpose::Baseline, route: Route::Activity },
    Registration { id: "yugabyte.operational", purpose: RecipePurpose::Baseline, route: Route::Operational },
    Registration::snapshot("yugabyte.statement-counters", Recipe::Statements).baseline(),
    Registration::snapshot("yugabyte.statement-definitions", Recipe::Definitions),
    Registration::snapshot("yugabyte.retained-plans", Recipe::Plans),
    Registration::snapshot("yugabyte.settings", Recipe::Schema(schema::Kind::Settings)).baseline(),
    Registration::snapshot("yugabyte.tables", Recipe::Schema(schema::Kind::Tables)).baseline(),
    Registration::snapshot("yugabyte.indexes", Recipe::Schema(schema::Kind::Indexes)).baseline(),
    Registration::snapshot("yugabyte.columns", Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Columns))),
    Registration::snapshot("yugabyte.foreign-keys", Recipe::Schema(schema::Kind::Catalog(catalog::Kind::ForeignKeys))),
    Registration::snapshot("yugabyte.check-constraints", Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Checks))),
    Registration::snapshot("yugabyte.objects", Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Objects))),
    Registration::snapshot("yugabyte.triggers", Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Triggers))),
    Registration::snapshot("yugabyte.partitions", Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Partitions))),
    Registration::snapshot("yugabyte.statistics", Recipe::Schema(schema::Kind::Catalog(catalog::Kind::Statistics))),
];

pub(crate) fn declarations() -> Vec<RecipeDescriptor> {
    REGISTRATIONS.iter().map(Registration::descriptor).collect()
}

pub(crate) fn open(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
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
