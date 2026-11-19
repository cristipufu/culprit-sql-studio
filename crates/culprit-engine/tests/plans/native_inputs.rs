//! The SDK compares existing supplied native bodies, without credentials, a recovery read or application execution.
use super::*;
use culprit_core::{CompareContext, t};
use culprit_engine::{
    Workspace,
    export::{ExportOptions, diff_html, diff_markdown},
    plans::compare_plans,
};

fn pair() -> (Plan, Plan) {
    (
        parse_plan(include_str!("../../../providers/culprit-mssql/tests/data/scalar-reuse/lookup.sqlplan")).unwrap(),
        parse_plan(include_str!("../../../providers/culprit-mssql/tests/data/scalar-reuse/scan.sqlplan")).unwrap(),
    )
}

fn observed(diff: &culprit_core::PlanDiff) -> bool {
    diff.scenarios.iter().any(|scenario| {
        scenario.detail.as_deref().is_some_and(|detail| detail.contains(&t!("core.plan_compare.scalar_reuse.limit")))
    })
}

fn assert_private_values_absent(text: &str) {
    for private in ["314159", "271828", "ParameterCompiledValue", "ParameterRuntimeValue", "ShowPlanXML"] {
        assert!(!text.contains(private), "{private}");
    }
}

#[test]
fn sdk_json_and_human_exports_preserve_the_native_same_input_comparison_without_literals() {
    let (a, b) = pair();
    let original_a = a.raw.clone();
    let cached_scan =
        parse_plan(&b.raw.as_ref().unwrap().replacen("RetrievedFromCache=\"false\"", "RetrievedFromCache=\"true\"", 1))
            .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let workspace = Workspace::open_in(directory.path());
    for b in [b, cached_scan] {
        let original_b = b.raw.clone();
        let comparison = workspace.diff_plans(&a, "Reused", &b, "Scan", None);
        assert!(observed(&comparison));
        assert_private_values_absent(&serde_json::to_string(&comparison).unwrap());
        let title = t!("core.plan_compare.scalar_reuse.title", reused = "Reused", scan = "Scan");
        for text in [
            diff_html(&a, &b, &comparison, "Reused", "Scan", &ExportOptions::default()),
            diff_markdown(&a, &b, &comparison, "Reused", "Scan", &ExportOptions::default()),
        ] {
            assert!(text.contains(&title));
            assert!(text.contains(&t!("core.plan_compare.scalar_reuse.limit")));
            assert_private_values_absent(&text);
        }
        assert_eq!(b.raw, original_b);
    }
    assert_eq!(a.raw, original_a, "comparison cannot rewrite the original native payload it authenticates");
}

#[test]
fn imported_json_flags_redaction_and_edited_runtime_cannot_supply_the_missing_native_pair() {
    let (a, b) = pair();
    let mut json = serde_json::to_value(&a).unwrap();
    json["raw"] = serde_json::Value::Null;
    json["runtime_input_relation"] = "same".into();
    let detached = serde_json::from_value::<Plan>(json).unwrap();
    let mut edited = a.clone();
    edited.root.children[0].children[1].runtime.as_mut().unwrap().logical_reads = Some(300_000.0);
    for unavailable in [detached, culprit_core::redact::redact_plan(&a), edited] {
        let comparison = compare_plans(&unavailable, "Reused", &b, "Scan", &CompareContext::default());
        assert!(!observed(&comparison));
        assert!(!comparison.headlines.is_empty(), "ordinary plan comparison remains available");
        assert_private_values_absent(&serde_json::to_string(&comparison).unwrap());
    }
}

#[test]
fn estimated_and_other_engine_plans_keep_the_ordinary_comparison_without_a_scalar_reuse_verdict() {
    let (a, b) = pair();
    let mut estimated = a.clone();
    estimated.kind = PlanKind::Estimated;
    assert!(!observed(&compare_plans(&estimated, "Estimated", &b, "Actual", &CompareContext::default())));
    let mut other_a = a;
    let mut other_b = b;
    other_a.engine = Engine::Postgres;
    other_b.engine = Engine::Postgres;
    let actual = compare_plans(&other_a, "A", &other_b, "B", &CompareContext::default());
    let ordinary = culprit_core::plan_diff_in(&other_a, "A", &other_b, "B", &CompareContext::default());
    assert!(!observed(&actual));
    assert_eq!(serde_json::to_value(actual).unwrap(), serde_json::to_value(ordinary).unwrap());
}
