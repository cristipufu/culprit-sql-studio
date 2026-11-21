//! A structural expectation for guard tests. It authenticates no cost, source or built-in approval.
use super::*;
pub fn baseline_profile_expectation(
    request: &CollectionRequest,
    context: &NativeReadContext,
    active: impl Fn() -> bool + Send + Sync + 'static,
) -> SourceProfileAdmission {
    let mut receipt = investigation_fixture().intent.qualification;
    receipt.recipe_id = "fixture.baseline-profile-binding".into();
    receipt.purpose = RecipePurpose::Baseline;
    receipt.selector = RecipeSelector::Collection(TelemetryCollection::Activity);
    receipt.scope = RecipeCostScope::NativeRead(SourceReadExecutionScope::Local(
        LocalResourceReadScope::from_context(context).unwrap(),
    ));
    receipt.tested_budget = request.budget.clone();
    SourceProfileAdmission::new(digest(b"controlled-profile-route"), receipt, request.budget.clone(), active).unwrap()
}
