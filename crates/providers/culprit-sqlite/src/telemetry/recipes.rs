//! One native file catalog owns declaration and selection. Availability still depends on the actual file.
use super::*;

pub(super) struct Registration {
    id: &'static str,
    pub dataset: SnapshotDataset,
    purpose: RecipePurpose,
}
impl Registration {
    const fn new(id: &'static str, dataset: SnapshotDataset, purpose: RecipePurpose) -> Self {
        Self { id, dataset, purpose }
    }
    fn selector(&self) -> RecipeSelector {
        RecipeSelector::Collection(TelemetryCollection::Snapshot(self.dataset))
    }
    fn descriptor(&self) -> RecipeDescriptor {
        RecipeDescriptor {
            id: self.id.into(),
            version: 1,
            purpose: self.purpose,
            selector: self.selector(),
            subjects: vec![RecipeSubject::Database],
            budget: CollectionBudget::upper_limits(),
            cost: RecipeCostQualification::Unqualified,
        }
    }
    pub fn bounded_request(&self, requested: &CollectionRequest) -> CollectionRequest {
        let mut request = requested.clone();
        request.budget = request.budget.intersection(&self.descriptor().budget);
        request
    }
}

const REGISTRATIONS: &[Registration] = &[
    Registration::new("sqlite.settings", SnapshotDataset::Settings, RecipePurpose::Baseline),
    Registration::new("sqlite.operational", SnapshotDataset::Operational, RecipePurpose::Baseline),
    Registration::new("sqlite.tables", SnapshotDataset::Tables, RecipePurpose::Investigation),
    Registration::new("sqlite.indexes", SnapshotDataset::Indexes, RecipePurpose::Investigation),
    Registration::new("sqlite.columns", SnapshotDataset::Columns, RecipePurpose::Investigation),
    Registration::new("sqlite.foreign-keys", SnapshotDataset::ForeignKeys, RecipePurpose::Investigation),
    Registration::new("sqlite.objects", SnapshotDataset::Objects, RecipePurpose::Investigation),
    Registration::new("sqlite.triggers", SnapshotDataset::Triggers, RecipePurpose::Investigation),
    Registration::new("sqlite.statistics", SnapshotDataset::Statistics, RecipePurpose::Investigation),
    Registration::new("sqlite.sequences", SnapshotDataset::Sequences, RecipePurpose::Investigation),
];

pub(crate) fn declarations() -> Vec<RecipeDescriptor> {
    REGISTRATIONS.iter().map(Registration::descriptor).collect()
}

pub(super) fn select(request: &CollectionRequest) -> Option<&'static Registration> {
    if !matches!(request.subject, CollectionSubject::Database) {
        return None;
    }
    REGISTRATIONS.iter().find(|recipe| recipe.selector().matches(request))
}

#[cfg(test)]
mod tests;
