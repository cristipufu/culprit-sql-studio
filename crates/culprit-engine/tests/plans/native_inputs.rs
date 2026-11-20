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
fn different_native_statistics_refresh_context_keeps_only_same_input_observed_work_with_its_limits() {
    let (a, b) = pair();
    let with_statistics = |plan: &Plan, at: &str| {
        let raw = plan.raw.as_deref().unwrap();
        let marker = "<QueryPlan>";
        assert!(raw.contains(marker));
        let metadata = format!(
            r#"<OptimizerStatsUsage><StatisticsInfo Database="[app]" Schema="[dbo]" Table="[rows]" Statistics="[ix_bucket]" LastUpdate="{at}" ModificationCount="0" SamplingPercent="100"/></OptimizerStatsUsage>"#
        );
        parse_plan(&raw.replacen(marker, &format!("{marker}{metadata}"), 1)).unwrap()
    };
    let a = with_statistics(&a, "2020-01-01T11:00:00.003");
    let b = with_statistics(&b, "2020-01-01T12:00:00.003");
    let original_a = a.raw.clone();
    let original_b = b.raw.clone();
    let comparison = compare_plans(&a, "Original", &b, "After statistics", &CompareContext::default());
    assert!(observed(&comparison));
    let scenario = comparison
        .scenarios
        .iter()
        .find(|scenario| {
            scenario
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains(&t!("core.plan_compare.scalar_reuse.limit")))
        })
        .unwrap();
    assert_eq!((scenario.a, scenario.b), (Some(3), Some(5)));
    let expected = format!(
        "{} {}",
        t!(
            "core.plan_compare.scalar_reuse.detail",
            reused = "Original",
            scan = "After statistics",
            executions = culprit_core::format::count(10_000.0),
            lookup_reads = culprit_core::format::count(30_000.0),
            scan_reads = culprit_core::format::count(2_000.0)
        ),
        t!("core.plan_compare.scalar_reuse.limit")
    );
    assert_eq!(scenario.detail.as_deref(), Some(expected.as_str()));
    assert_private_values_absent(&serde_json::to_string(&comparison).unwrap());
    assert_eq!(a.raw, original_a);
    assert_eq!(b.raw, original_b);
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
fn sdk_equal_compiled_inputs_preserve_only_measured_work_and_qualified_guidance_through_private_exports() {
    let (lookup, scan) = pair();
    let equal = parse_plan(&lookup.raw.as_ref().unwrap().replacen(
        "ParameterCompiledValue=\"(271828)\"",
        "ParameterCompiledValue=\"(314159)\"",
        1,
    ))
    .unwrap();
    assert_eq!(equal.facts().parameter_context.as_ref().unwrap().parameters[0].integer_values_equal, Some(true));
    let original = equal.raw.clone();
    let directory = tempfile::tempdir().unwrap();
    let workspace = Workspace::open_in(directory.path());
    let comparison = workspace.diff_plans(&equal, "Lookup", &scan, "Scan", None);
    assert!(observed(&comparison));
    let scenario = comparison
        .scenarios
        .iter()
        .find(|scenario| {
            scenario
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains(&t!("core.plan_compare.scalar_reuse.equal_compile_input")))
        })
        .unwrap();
    assert_eq!((scenario.a, scenario.b), (Some(3), Some(5)));
    let expected_detail = format!(
        "{} {} {} {}",
        t!(
            "core.plan_compare.scalar_reuse.detail",
            reused = "Lookup",
            scan = "Scan",
            executions = culprit_core::format::count(10_000.0),
            lookup_reads = culprit_core::format::count(30_000.0),
            scan_reads = culprit_core::format::count(2_000.0)
        ),
        t!("core.plan_compare.scalar_reuse.equal_compile_input"),
        t!("core.plan_compare.scalar_reuse.limit"),
        t!("core.plan_compare.scalar_reuse.equal_investigation")
    );
    assert_eq!(scenario.detail.as_deref(), Some(expected_detail.as_str()));
    let json = serde_json::to_string(&comparison).unwrap();
    let restored: culprit_core::PlanDiff = serde_json::from_str(&json).unwrap();
    assert_eq!(serde_json::to_value(&restored).unwrap(), serde_json::to_value(&comparison).unwrap());
    assert_private_values_absent(&json);
    for text in [
        diff_html(&equal, &scan, &comparison, "Lookup", "Scan", &ExportOptions::default()),
        diff_markdown(&equal, &scan, &comparison, "Lookup", "Scan", &ExportOptions::default()),
    ] {
        assert!(text.contains(&t!("core.plan_compare.scalar_reuse.title", reused = "Lookup", scan = "Scan")));
        assert!(text.contains(&t!("core.plan_compare.scalar_reuse.equal_compile_input")));
        assert!(text.contains(&t!("core.plan_compare.scalar_reuse.equal_investigation")));
        assert_private_values_absent(&text);
    }
    assert_eq!(equal.raw, original, "native comparison and export cannot rewrite the original input evidence");
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
