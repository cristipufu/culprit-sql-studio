use super::*;
use crate::{resources::test_support::replace, value::Val};

pub(crate) fn spill_columns() -> Vec<Rec> {
    STATISTICS.map(|statistic| Rec::of(&[("name", Val::Text(format!("{statistic}_spills")))])).into()
}

#[tokio::test]
async fn optional_spill_columns_are_qualified_once_without_another_cache_walk() {
    use crate::resources::test_support::FakeRead;
    for available in [false, true] {
        let mut reader = FakeRead::new(vec![Ok(if available { spill_columns() } else { vec![] })]);
        let layout = Layout::read(&mut reader).await.unwrap();
        let sql = select_sql(&layout);
        assert_eq!(reader.calls.len(), 1);
        assert_eq!(reader.calls[0].limit, 5);
        assert!(reader.calls[0].sql.contains("sys.all_columns"));
        assert!(!reader.calls[0].sql.contains("FROM sys.dm_exec_query_stats"));
        for statistic in STATISTICS {
            let column = format!("{statistic}_spills");
            if available {
                assert!(sql.contains(&format!("qs.{column}")));
                assert!(!sql.contains(&format!("NULL AS bigint) AS {column}")));
            } else {
                assert!(sql.contains(&format!("CAST(NULL AS bigint) AS {column}")));
                assert!(!sql.contains(&format!("qs.{column}")));
            }
        }
        assert!(sql.contains("TOP (@P1)"));
    }
    let partial = vec![spill_columns()[0].clone()];
    let duplicate = vec![spill_columns()[0].clone(), spill_columns()[0].clone()];
    for rows in [partial, duplicate, vec![Rec::of(&[("name", Val::Text("unselected-column".into()))])]] {
        assert!(Layout::read(&mut FakeRead::new(vec![Ok(rows)])).await.is_err());
    }
}

#[test]
fn spill_pages_preserve_completed_execution_units_zero_unknown_and_exact_distribution_counters() {
    let native = row();
    let measured = decode(&native).unwrap();
    let spills = &measured.counters[&CachedRuntimeMetric::SpilledPages];
    assert_eq!(spills.total, "9007199254740993");
    assert_eq!(spills.last, "9007199254740993");
    for value in [Val::Null, Val::Int(0)] {
        let changes = STATISTICS.map(|statistic| (format!("{statistic}_spills"), value.clone()));
        let references = changes.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>();
        let runtime = decode(&replace(&native, &references)).unwrap();
        assert!(runtime.is_valid());
        match value {
            Val::Null => assert!(!runtime.counters.contains_key(&CachedRuntimeMetric::SpilledPages)),
            _ => assert_eq!(runtime.counters[&CachedRuntimeMetric::SpilledPages].total, "0"),
        }
        assert_eq!(runtime.context.identity, measured.context.identity);
        assert_eq!(runtime.executions, measured.executions);
        assert_eq!(runtime.counters[&CachedRuntimeMetric::Rows], measured.counters[&CachedRuntimeMetric::Rows]);
    }
    for value in [Val::Null, Val::Int(-1), Val::Float(1.0)] {
        assert!(decode(&replace(&native, &[("total_spills", value)])).is_err());
    }
    let mut without_required = measured.clone();
    without_required.counters.remove(&CachedRuntimeMetric::WorkerMicroseconds);
    assert!(!without_required.is_valid());
}

pub(crate) fn row() -> Rec {
    let context = crate::grants::tests::row();
    let mut fields = context.columns().map(|(name, value)| (name.to_string(), value.clone())).collect::<Vec<_>>();
    fields.push(("query_plan_hash".into(), Val::Text("0x0123456789ABCDEF".into())));
    for metric in CachedRuntimeMetric::ALL {
        for statistic in ["total", "last", "min", "max"] {
            fields.push((format!("{statistic}_{}", native_suffix(metric)), Val::Int(9_007_199_254_740_993)));
        }
    }
    Rec::of(&fields.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>())
}
#[test]
fn native_runtime_keeps_exact_counters_identity_and_clock_semantics() {
    let runtime = decode(&row()).unwrap();
    assert_eq!(runtime.executions.as_deref(), Some("2"));
    assert_eq!(runtime.counters[&CachedRuntimeMetric::WorkerMicroseconds].total, "9007199254740993");
    assert_eq!(runtime.context.identity.created_without_offset, "2020-01-01T10:00:00.0030000");
    assert_eq!(runtime.last_execution_without_offset.as_deref(), Some("2020-01-01T10:01:00.0070000"));
    assert_eq!(runtime.reset, culprit_core::ResetMarker::Unknown);
    assert!(!serde_json::to_string(&runtime).unwrap().contains("SELECT"));
    for value in [Val::Null, Val::Int(-1), Val::Float(1.0)] {
        assert!(decode(&replace(&row(), &[("total_worker_time", value)])).is_err());
    }
}
#[test]
fn zero_native_plan_handles_never_qualify_placeholder_counters_as_measurements() {
    let runtime = decode(&replace(&row(), &[("plan_handle", Val::Bytes(vec![0; 64]))])).unwrap();
    assert!(runtime.is_valid());
    assert!(!runtime.measured());
    assert!(runtime.executions.is_none() && runtime.counters.is_empty());
    let mut fabricated = runtime.clone();
    fabricated.executions = Some("0".into());
    assert!(!fabricated.is_valid());
}
#[test]
fn recompiled_entries_and_culprit_provenance_keep_native_ownership() {
    let first = decode(&row()).unwrap();
    let second = decode(&replace(
        &row(),
        &[("plan_generation_num", Val::Int(2)), ("text_prefix", Val::Text("SELECT /* culprit */ 1".into()))],
    ))
    .unwrap();
    assert_ne!(first.context.identity.key().unwrap(), second.context.identity.key().unwrap());
    assert_eq!(second.context.origin, StatementOrigin::Culprit);
    assert!(second.counters.contains_key(&CachedRuntimeMetric::Rows));
}

fn summary_row() -> Rec {
    Rec::of(&[
        ("query_hash", Val::Text("0x0123456789ABCDEF".into())),
        ("unqualified_entries", Val::Int(0)),
        ("executions", Val::Int(9_007_199_254_740_993)),
        ("duration_us", Val::Int(4000)),
        ("maximum_us", Val::Int(3000)),
        ("minimum_us", Val::Int(1000)),
        ("worker_us", Val::Int(2000)),
        ("logical_reads", Val::Int(100)),
        ("physical_reads", Val::Int(3)),
        ("row_total", Val::Int(20)),
        ("plan_count", Val::Int(2)),
        ("first_seen", Val::Text("2020-01-01T06:00:00Z".into())),
        ("last_seen", Val::Text("2020-01-01T06:01:00Z".into())),
        ("age_seconds", Val::Int(-1)),
    ])
}
#[test]
fn summary_counts_remain_exact_until_the_application_projects_them() {
    let summary = Summary::decode(&summary_row()).unwrap();
    let counts = summary.counts.as_ref().unwrap();
    assert_eq!(counts.executions, 9_007_199_254_740_993);
    assert_eq!(counts.duration_us, 4000);
    assert_eq!(summary.oldest_entry_age_seconds, -1, "native clock skew stays explicit");
    for (column, value) in [("executions", Val::Null), ("worker_us", Val::Int(-1)), ("logical_reads", Val::Float(3.0))]
    {
        assert!(Summary::decode(&replace(&summary_row(), &[(column, value)])).is_err());
    }
}
#[tokio::test]
async fn unknown_native_counters_cannot_become_zero_cost_workload_or_trigger_detail_reads() {
    let row = replace(&summary_row(), &[("unqualified_entries", Val::Int(1)), ("executions", Val::Null)]);
    assert!(Summary::decode(&row).unwrap().counts.is_none());
    let mut reader = crate::resources::test_support::FakeRead::new(vec![Ok(vec![row])]);
    let result =
        crate::workload::from_plan_cache(&mut reader, &culprit_core::SnapshotOptions::default()).await.unwrap();
    assert!(result.workload.queries.is_empty());
    assert_eq!(reader.calls.len(), 1);
    assert!(
        result.coverage.outcomes().iter().any(|source| source.source == crate::sources::PLAN_CACHE.name
            && source.state == culprit_core::SourceState::Partial)
    );
}
