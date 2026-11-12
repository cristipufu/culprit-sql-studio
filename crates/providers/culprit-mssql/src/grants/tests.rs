use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};

pub(crate) fn layout_row(visible: bool, available: &[&str]) -> Rec {
    let mut fields = vec![("metadata_visibility", Val::Int(i64::from(visible)))];
    fields.extend(COLUMNS.into_iter().map(|name| (name, Val::Int(i64::from(available.contains(&name))))));
    Rec::of(&fields)
}
pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("plan_handle", Val::Bytes(vec![1; 64])),
        ("sql_handle", Val::Bytes(vec![2; 64])),
        ("statement_start_offset", Val::Int(0)),
        ("statement_end_offset", Val::Int(-1)),
        ("creation_time", Val::Text("2020-01-01T10:00:00.003".into())),
        ("plan_generation_num", Val::Int(1)),
        ("database_id", Val::Int(5)),
        ("query_hash", Val::Text("0x0123456789ABCDEF".into())),
        ("execution_count", Val::Int(2)),
        ("last_execution", Val::Text("2020-01-01T10:01:00.007".into())),
        ("total_grant_kb", Val::Int(20)),
        ("total_used_grant_kb", Val::Int(10)),
        ("last_grant_kb", Val::Int(12)),
        ("last_used_grant_kb", Val::Int(6)),
        ("min_grant_kb", Val::Int(8)),
        ("min_used_grant_kb", Val::Int(4)),
        ("max_grant_kb", Val::Int(12)),
        ("max_used_grant_kb", Val::Int(6)),
        ("source_text_bytes", Val::Int(16)),
        ("text_complete", Val::Int(1)),
        ("text_prefix", Val::Text("SELECT 1".into())),
        ("engine_internal", Val::Int(0)),
        ("cloud_internal", Val::Int(0)),
    ])
}
fn layout() -> Layout {
    Layout { columns: COLUMNS.iter().map(|name| name.to_string()).collect() }
}
fn fact(values: &[(&str, Val)]) -> StatementGrantStatistics {
    decode(&replace(&row(), values), &layout()).unwrap()
}

#[tokio::test]
async fn unavailable_columns_require_visibility_before_being_called_not_applicable() {
    let mut hidden = FakeRead::new(vec![Ok(vec![layout_row(false, &[])])]);
    let error = Layout::read(&mut hidden).await.err().unwrap();
    assert_eq!(error.code, culprit_core::ErrorCode::PermissionDenied);
    assert!(error.hint.unwrap().contains("VIEW DEFINITION"));
    let mut absent = FakeRead::new(vec![Ok(vec![layout_row(true, &[])])]);
    assert!(Layout::read(&mut absent).await.unwrap().is_none());
    let mut subset = FakeRead::new(vec![Ok(vec![layout_row(true, &["total_grant_kb"])])]);
    let subset = Layout::read(&mut subset).await.unwrap().unwrap();
    assert!(!subset.can_summarize());
    assert!(select_sql(&subset, false).contains("CAST(NULL AS bigint) AS last_grant_kb"));
}

#[test]
fn native_grants_preserve_ticks_exact_bigints_offsets_and_unknown_columns() {
    let native = fact(&[("total_grant_kb", Val::Int(9_007_199_254_740_993)), ("last_used_grant_kb", Val::Null)]);
    assert_eq!(native.total_grant_kibibytes.as_deref(), Some("9007199254740993"));
    assert_eq!(native.context.identity.created_without_offset, "2020-01-01T10:00:00.0030000");
    assert_eq!(native.last_execution_without_offset.as_deref(), Some("2020-01-01T10:01:00.0070000"));
    assert_eq!(native.context.identity.statement_end_bytes, -1);
    assert_eq!(native.last_used_kibibytes, None);
    assert_eq!(native.reset, culprit_core::ResetMarker::Unknown);
    let mut recompiled = native.clone();
    recompiled.context.identity.plan_generation = "2".into();
    assert_ne!(native.context.identity.key().unwrap(), recompiled.context.identity.key().unwrap());
    for values in [
        vec![("total_grant_kb", Val::Int(-1))],
        vec![("execution_count", Val::Float(2.0))],
        vec![("plan_handle", Val::Null)],
        vec![("statement_end_offset", Val::Int(-2))],
    ] {
        assert!(decode(&replace(&row(), &values), &layout()).is_err());
    }
    let exported = serde_json::to_string(&native).unwrap();
    assert!(!exported.contains("SELECT"), "provenance text is inspected but never exported");
}

#[test]
fn truncated_native_text_preserves_completed_monitoring_markers_without_classifying_unknowns_as_application() {
    let template = "a".repeat(64);
    let prefix = format!("SELECT /* culprit */ /* culprit:monitor */ /* culprit:template:{template} */ N'unfinished");
    let own = fact(&[("text_prefix", Val::Text(prefix)), ("text_complete", Val::Int(0))]);
    assert_eq!(own.context.origin, StatementOrigin::CulpritMonitoring);
    assert_eq!(own.context.monitoring_template_id.as_deref(), Some(template.as_str()));
    for prefix in [
        "SELECT N'/* culprit */' AS x, N'cut",
        "SELECT [/* culprit */] FROM [cut",
        "SELECT /* outside /* culprit */",
        "SELECT 1 -- /* culprit */",
    ] {
        let unknown = fact(&[("text_prefix", Val::Text(prefix.into())), ("text_complete", Val::Int(0))]);
        assert_eq!(unknown.context.origin, StatementOrigin::Unknown, "{prefix}");
    }
    assert_eq!(
        fact(&[("text_prefix", Val::Null), ("engine_internal", Val::Int(1))]).context.origin,
        StatementOrigin::EngineInternal
    );
}

#[test]
fn application_aggregation_weights_executions_and_never_averages_unrelated_last_values() {
    let first = fact(&[]);
    let second = fact(&[
        ("execution_count", Val::Int(1)),
        ("total_grant_kb", Val::Int(40)),
        ("total_used_grant_kb", Val::Int(20)),
        ("max_grant_kb", Val::Int(40)),
        ("last_grant_kb", Val::Int(40)),
        ("last_execution", Val::Text("2020-01-01T11:00:00".into())),
    ]);
    let mut own = first.clone();
    own.context.origin = StatementOrigin::Culprit;
    let summary = projection::summarize(&[first.clone(), second, own]).unwrap();
    assert!(summary.complete);
    let result = &summary.grants[0];
    assert_eq!(result.executions, 3.0);
    assert_eq!(result.avg_granted_kb, 20.0);
    assert_eq!(result.avg_used_kb, 10.0);
    assert_eq!(result.max_granted_kb, 40.0);
    assert_eq!(result.last_granted_kb, None, "a later native start is not proof of a later completion");
    assert_eq!(result.last_used_kb, None);
    let single = projection::summarize(std::slice::from_ref(&first)).unwrap();
    assert_eq!(single.grants[0].last_granted_kb, Some(12.0));
    let agreed = projection::summarize(&[first.clone(), first]).unwrap();
    assert_eq!(agreed.grants[0].last_used_kb, Some(6.0));
    let missing = projection::summarize(&[fact(&[("total_used_grant_kb", Val::Null)])]).unwrap();
    assert!(!missing.complete);
    assert!(missing.grants.is_empty());
    let zero = projection::summarize(&[fact(&[("total_grant_kb", Val::Int(0)), ("total_used_grant_kb", Val::Int(0))])])
        .unwrap();
    assert_eq!(zero.grants[0].avg_granted_kb, 0.0);
}

#[tokio::test]
async fn selected_workload_reads_zero_grants_through_the_same_native_decoder_and_parameterizes_hashes() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![layout_row(true, &COLUMNS)]),
        Ok(vec![replace(&row(), &[("total_grant_kb", Val::Int(0)), ("total_used_grant_kb", Val::Int(0))])]),
    ]);
    let selected = vec!["0x0123456789ABCDEF".to_string()];
    let summary = application(&mut reader, ApplicationSelection::Hashes(&selected)).await.unwrap().unwrap();
    assert_eq!(summary.grants[0].avg_granted_kb, 0.0);
    assert!(!reader.calls[1].sql.contains(&selected[0]));
    assert!(!reader.calls[1].sql.contains("total_grant_kb>0"));
    assert!(reader.calls[1].sql.contains("CAST(pa.value AS int)=DB_ID()"));
    assert!(!reader.calls[1].sql.contains("NOT LIKE"));
    assert_eq!(reader.calls[1].parameters.len(), 2);
    assert!(hashes_parameter(&["0x0);DROP TABLE x;--".into()]).is_err());
    assert!(hashes_parameter(&vec![selected[0].clone(); MAX_HASH_SELECTORS + 1]).is_err());
}

#[tokio::test]
async fn selected_workload_batches_hashes_without_losing_the_tail_or_double_counting_case_variants() {
    let mut hashes = (0..201).map(|n| format!("0x{n:016X}")).collect::<Vec<_>>();
    hashes.extend(["0x00000000000000c8".into(), "0x00000000000000C8".into()]);
    let first = replace(&row(), &[("query_hash", Val::Text(hashes[0].clone()))]);
    let last = replace(&row(), &[("query_hash", Val::Text(hashes[200].clone()))]);
    let mut reader = FakeRead::new(vec![Ok(vec![layout_row(true, &COLUMNS)]), Ok(vec![first]), Ok(vec![last])]);
    let result = application(&mut reader, ApplicationSelection::Hashes(&hashes)).await.unwrap().unwrap();
    assert_eq!(result.grants.len(), 2);
    assert!(result.grants.iter().any(|grant| grant.query_hash == hashes[200]));
    assert_eq!(reader.calls.len(), 3);
    let Parameter::Text(second) = &reader.calls[2].parameters[1] else { panic!("XML selection") };
    assert_eq!(second.matches("<h ").count(), 1, "duplicate native hashes must not be collected in later batches");
}

#[test]
fn zero_native_handles_keep_unqualified_grant_counters_unknown() {
    let native = fact(&[("plan_handle", Val::Bytes(vec![0; 64]))]);
    assert!(native.executions.is_none());
    assert!(native.total_grant_kibibytes.is_none() && native.total_used_kibibytes.is_none());
    let summary = projection::summarize(&[native]).unwrap();
    assert!(!summary.complete);
    assert!(summary.grants.is_empty());
}
