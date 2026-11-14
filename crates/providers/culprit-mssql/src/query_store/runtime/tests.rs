use super::*;
use crate::{query_store::tests::summary_row, resources::test_support::replace, value::Val};

fn costs(row: &Rec) -> ExecutionCosts {
    PlanSummary::decode(row).unwrap().costs
}

#[test]
fn native_outcomes_and_abort_mean_use_the_same_exact_execution_population() {
    let row = replace(
        &summary_row(),
        &[("aborted", Val::Int(1)), ("aborted_us", Val::Float(3000.0)), ("log_bytes", Val::Float(24.0))],
    );
    let costs = costs(&row);
    let observed = costs.query_costs(Some(48.0));
    assert_eq!(costs.executions, 2);
    assert_eq!(observed.failed, Some(1.0));
    assert_eq!(observed.aborted, Some(1.0));
    assert_eq!(observed.aborted_mean_ms, Some(3.0));
    assert_eq!(observed.log_bytes, Some(24.0));
    assert_eq!(observed.log_share, Some(0.5));
}

#[test]
fn unsupported_log_column_never_appears_in_the_native_statement() {
    let sql = summary_sql(false, false);
    assert!(!sql.contains("rs.avg_log_bytes_used"));
    assert!(sql.contains("CAST(NULL AS float) AS log_bytes"));
    assert!(sql.contains(super::super::window_sql::MEMBERSHIP));
    let supported = summary_sql(false, true);
    assert!(supported.contains("rs.count_executions>0 AND rs.avg_log_bytes_used IS NULL"));
    assert!(!supported.contains("total_log_bytes"), "no second global runtime relation/denominator read");
    let unknown = costs(&summary_row());
    assert!(unknown.is_complete(), "an unavailable optional metric is distinct from lost population members");
    assert_eq!(unknown.query_costs(None).log_bytes, None);
}

#[test]
fn missing_native_log_members_do_not_form_a_complete_denominator() {
    let known = costs(&replace(&summary_row(), &[("log_bytes", Val::Float(12.0))]));
    let missing = costs(&replace(&summary_row(), &[("log_complete", Val::Int(0))]));
    assert!(!missing.is_complete());
    let combined = ExecutionCosts::aggregate([&known, &missing]).unwrap();
    assert_eq!(combined.executions, 4);
    assert_eq!(combined.log_bytes, None);
    assert!(!combined.is_complete());
    let own = known.query_costs(combined.log_bytes);
    assert_eq!(own.log_bytes, Some(12.0));
    assert_eq!(own.log_share, None);
}

#[test]
fn unknown_native_outcomes_preserve_positive_counts_without_inventing_absent_classes() {
    let partial = costs(&replace(&summary_row(), &[("known_executions", Val::Int(1))]));
    assert!(!partial.is_complete());
    let observed = partial.query_costs(None);
    assert_eq!(observed.failed, Some(1.0));
    assert_eq!(observed.aborted, None);
    let unknown = costs(&replace(&summary_row(), &[("known_executions", Val::Int(0)), ("failed", Val::Int(0))]));
    assert_eq!(unknown.query_costs(None).failed, None);
    assert_eq!(unknown.query_costs(None).aborted, None);
}

#[test]
fn impossible_native_outcomes_and_log_completeness_are_rejected() {
    for changes in [
        vec![("failed", Val::Int(2)), ("aborted", Val::Int(1))],
        vec![("known_executions", Val::Int(3))],
        vec![("known_executions", Val::Int(0))],
        vec![("aborted", Val::Int(-1))],
        vec![("log_bytes", Val::Float(-1.0))],
        vec![("log_bytes", Val::Float(f64::INFINITY))],
        vec![("log_complete", Val::Int(0)), ("log_bytes", Val::Float(12.0))],
        vec![("log_complete", Val::Int(2))],
    ] {
        assert!(PlanSummary::decode(&replace(&summary_row(), &changes)).is_err(), "{changes:?}");
    }
}

#[test]
fn count_precision_and_aggregation_overflow_do_not_become_rounded_failure_ratios() {
    let exact = 9_007_199_254_740_993i64;
    let large =
        costs(&replace(&summary_row(), &[("executions", Val::Int(exact)), ("known_executions", Val::Int(exact))]));
    assert_eq!(large.executions, exact as u64);
    assert!(!large.is_complete());
    assert_eq!(large.query_costs(None).failed, None);
    assert_eq!(large.query_costs(None).aborted, None);
    let maximal = costs(&replace(
        &summary_row(),
        &[("executions", Val::Int(i64::MAX)), ("known_executions", Val::Int(i64::MAX))],
    ));
    assert!(ExecutionCosts::aggregate([&maximal, &maximal, &maximal]).is_none());
    let huge = costs(&replace(&summary_row(), &[("log_bytes", Val::Float(f64::MAX))]));
    assert!(ExecutionCosts::aggregate([&huge, &huge]).is_none());
}
