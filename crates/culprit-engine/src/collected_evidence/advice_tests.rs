#![cfg(feature = "postgres")]
use super::*;
use culprit_core::{PlanHistory, PlanHistoryKind};

#[test]
fn missing_prerequisites_withhold_current_and_retained_plan_sql_after_native_annotation() {
    let plan = crate::plans::parse_native_plan(
        include_str!("../../tests/insights/pg-heap-fetches.json"),
        culprit_core::Engine::Postgres,
    )
    .unwrap();
    let native = serde_json::to_value(&plan.root).unwrap();
    let insight = plan.insights.as_ref().unwrap().items.iter().find(|insight| insight.rule == "heap-fetches").unwrap();
    assert!(insight.fix_sql.as_deref().is_some_and(|sql| sql.starts_with("VACUUM")));
    let original_fix = insight.fix.clone();
    let original_verify = insight.verify.clone();
    let scan: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/postgres-demo.json")).unwrap();
    let mut workload = scan.workload.unwrap();
    workload.queries.truncate(1);
    let query = &mut workload.queries[0];
    assert!(query.executions > 1.0);
    query.plan = Some(plan.clone());
    query.plan_history = Some(PlanHistory {
        source: "retained-native-plan".into(),
        kind: PlanHistoryKind::Cache,
        note: String::new(),
        captured_plan_id: Some("native-retained-plan".into()),
        captured_plan: Some(plan),
        source_message: None,
        note_message: None,
    });
    let mut limitations = vec![limit(None, CollectedAnalysisLimit::MissingSchema)];
    apply_executable_fix_policy(&mut [], Some(&mut workload), &mut limitations);
    let query = &workload.queries[0];
    for plan in query.plan.iter().chain(query.plan_history.as_ref().unwrap().captured_plan.iter()) {
        assert_eq!(serde_json::to_value(&plan.root).unwrap(), native);
        let insight =
            plan.insights.as_ref().unwrap().items.iter().find(|insight| insight.rule == "heap-fetches").unwrap();
        assert!(insight.fix_sql.is_none());
        assert_eq!(insight.fix, original_fix);
        assert_eq!(insight.verify, original_verify);
    }
    assert!(limitations.iter().any(|limit| limit.record_id.as_deref() == Some(query.id.as_str())
        && limit.reason == CollectedAnalysisLimit::ExecutableFixPrerequisitesUnknown));
}
