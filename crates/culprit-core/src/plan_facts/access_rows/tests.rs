use super::*;
use crate::PlanNodeFacts;

#[test]
fn native_access_estimates_and_actual_totals_roundtrip_without_filling_each_others_gaps() {
    for (estimate, actual) in
        [(Some(100.0), Some(10_000.0)), (Some(0.0), Some(0.0)), (Some(100.0), None), (None, Some(10_000.0))]
    {
        let rows = PlanAccessRows::new(estimate, actual).unwrap();
        let facts = PlanNodeFacts { access_rows: Some(rows), ..Default::default() };
        let reread: PlanNodeFacts = serde_json::from_slice(&serde_json::to_vec(&facts).unwrap()).unwrap();
        assert_eq!(reread.access_rows, Some(rows));
        assert_eq!(rows.estimated_per_execution, estimate);
        assert_eq!(rows.actual_total, actual);
    }
    assert!(PlanAccessRows::new(None, None).is_none());
}

#[test]
fn invalid_access_counts_cannot_authorize_an_actual_measurement() {
    for value in [-1.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        let invalid = PlanAccessRows { estimated_per_execution: Some(100.0), actual_total: Some(value) };
        assert!(!invalid.is_valid());
        let estimate_only = PlanAccessRows::new(Some(100.0), Some(value)).unwrap();
        assert_eq!(estimate_only.actual_total, None);
        assert!(PlanAccessRows::new(None, Some(value)).is_none());
    }
    let mut imported = serde_json::to_value(PlanNodeFacts::default()).unwrap();
    imported["access_rows"] = serde_json::json!({"estimated_per_execution": 100.0, "actual_total": -1.0});
    let facts: PlanNodeFacts = serde_json::from_value(imported).unwrap();
    let access = facts.access_rows.unwrap();
    assert_eq!(access.estimated_per_execution, Some(100.0));
    assert_eq!(access.actual_total, None);
}
