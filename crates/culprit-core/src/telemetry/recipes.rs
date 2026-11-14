//! Source providers own one registration catalog. Common scheduling never interprets engine identity as support.
use super::*;

pub const MAX_TELEMETRY_RECIPES: usize = 128;

/// A selected provider declares one bounded, unambiguous native source per selector and subject.
/// Empty means undeclared support and never authorizes a source from another provider's catalog.
pub fn recipe_catalog_is_valid(recipes: &[RecipeDescriptor]) -> bool {
    let mut ids = std::collections::BTreeSet::new();
    recipes.len() <= MAX_TELEMETRY_RECIPES
        && recipes.iter().enumerate().all(|(index, recipe)| {
            recipe.is_valid()
                && !matches!(recipe.selector, RecipeSelector::Collection(TelemetryCollection::Diagnostics))
                && ids.insert(&recipe.id)
                && recipes[..index].iter().all(|before| {
                    before.selector != recipe.selector
                        || before.subjects.iter().all(|subject| !recipe.subjects.contains(subject))
                })
        })
}

impl CollectionBudget {
    /// Native profile envelopes and caller limits both apply; intersect every independently enforced dimension.
    pub fn intersection(&self, other: &Self) -> Self {
        Self {
            page_rows: self.page_rows.min(other.page_rows),
            max_snapshot_rows: self.max_snapshot_rows.min(other.max_snapshot_rows),
            page_bytes: self.page_bytes.min(other.page_bytes),
            total_bytes: self.total_bytes.min(other.total_bytes),
            max_sql_bytes: self.max_sql_bytes.min(other.max_sql_bytes),
            max_plan_bytes: self.max_plan_bytes.min(other.max_plan_bytes),
            max_pages: self.max_pages.min(other.max_pages),
            max_queries: self.max_queries.min(other.max_queries),
            statement_timeout_ms: self.statement_timeout_ms.min(other.statement_timeout_ms),
            total_timeout_ms: self.total_timeout_ms.min(other.total_timeout_ms),
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "collection", rename_all = "kebab-case")]
#[ts(export)]
pub enum RecipeSelector {
    Collection(TelemetryCollection),
    FocusedPageWait,
}

#[cfg(test)]
mod tests;

impl RecipeSelector {
    pub fn matches(&self, request: &CollectionRequest) -> bool {
        match (self, &request.selection) {
            (Self::FocusedPageWait, CollectionSelection::FocusedPageWait { .. }) => true,
            (Self::Collection(_), CollectionSelection::FocusedPageWait { .. }) => false,
            (Self::Collection(collection), _) => *collection == request.collection(),
            _ => false,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "resource", rename_all = "kebab-case")]
#[ts(export)]
pub enum RecipeSubject {
    Database,
    Shared(SharedResourceKind),
}

impl RecipeSubject {
    pub fn accepts(&self, subject: &CollectionSubject) -> bool {
        match (self, subject) {
            (Self::Database, CollectionSubject::Database) => true,
            (Self::Shared(expected), CollectionSubject::Shared { resource }) => *expected == resource.kind,
            _ => false,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum RecipeCostQualification {
    /// Bounded rows, read-only SQL and a passing local fixture are not production work-cost qualification.
    Unqualified,
    /// Source-issued approval for one exact native context. Authentication and the selected source's pure
    /// native-cost verifier are mandatory; structural receipt/accounting validity never grants native admission.
    Measured(Box<RecipeCostReceipt>),
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum RecipePurpose {
    Baseline,
    Investigation,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RecipeDescriptor {
    pub id: String,
    pub version: u32,
    pub purpose: RecipePurpose,
    pub selector: RecipeSelector,
    pub subjects: Vec<RecipeSubject>,
    /// The same source-owned clamp supplies native construction and this declared upper envelope.
    pub budget: CollectionBudget,
    /// An implementation declaration does not establish live product/version/permission availability.
    pub cost: RecipeCostQualification,
}

impl RecipeDescriptor {
    pub fn is_valid(&self) -> bool {
        valid_key(&self.id)
            && self.version > 0
            && !self.subjects.is_empty()
            && self.subjects.len() <= 8
            && self.subjects.iter().enumerate().all(|(index, subject)| !self.subjects[..index].contains(subject))
            && self.budget.validate().is_ok()
            && match &self.cost {
                RecipeCostQualification::Unqualified => true,
                RecipeCostQualification::Measured(receipt) => receipt.matches_recipe(self),
            }
    }

    pub fn accepts(&self, request: &CollectionRequest) -> bool {
        self.is_valid()
            && self.selector.matches(request)
            && self.subjects.iter().any(|subject| subject.accepts(&request.subject))
    }
}
