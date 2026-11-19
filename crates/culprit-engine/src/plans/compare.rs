//! The existing native adapter supplies a borrowed comparison; JSON parameter flags supply no proof.
use culprit_core::{CompareContext, Plan, PlanDiff};

pub(super) fn compare(a: &Plan, name_a: &str, b: &Plan, name_b: &str, context: &CompareContext) -> PlanDiff {
    if let Some(pair) = native_inputs(a, b) {
        return culprit_core::plan_compare::compare_native_pair(pair, name_a, name_b, context);
    }
    culprit_core::plan_diff_in(a, name_a, b, name_b, context)
}

pub(crate) fn compare_redacted(a: &Plan, name_a: &str, b: &Plan, name_b: &str) -> PlanDiff {
    if let Some(pair) = native_inputs(a, b) {
        return culprit_core::plan_compare::compare_native_pair(pair, name_a, name_b, &CompareContext::default());
    }
    let (a, b) = (culprit_core::redact::redact_plan(a), culprit_core::redact::redact_plan(b));
    culprit_core::plan_diff_named(&a, name_a, &b, name_b)
}

fn native_inputs<'a>(a: &'a Plan, b: &'a Plan) -> Option<culprit_core::plan_compare::NativeRuntimeInputPair<'a>> {
    #[cfg(feature = "mssql")]
    if a.engine == culprit_core::Engine::Mssql && b.engine == a.engine {
        return Some(culprit_mssql::plan_xml::runtime_input_pair(a, b));
    }
    let _ = (a, b);
    None
}
