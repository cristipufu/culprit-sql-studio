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
pub(crate) fn platform() -> Rec {
    replace(&crate::instance::test_support::metadata(), &[("edition", Val::Int(5))])
}
pub(crate) fn authority(at: &str) -> Rec {
    replace(
        &crate::activity::database_context::tests::row(5),
        &[
            ("observed_at", Val::Text(at.into())),
            ("selected_database_id", Val::Int(5)),
            ("catalog_database_id", Val::Int(5)),
            ("database_name", Val::Text("sample".into())),
            ("catalog_database_name", Val::Text("sample".into())),
            ("incarnation", Val::Text("logical-guid".into())),
            ("physical_incarnation", Val::Text("physical-guid".into())),
        ],
    )
}
pub(crate) fn layout() -> Layout {
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
    assert!(subset.has("total_grant_kb"));
    assert!(!subset.has("total_used_grant_kb"));
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
    assert!(exported.contains("SELECT"), "only the complete original statement is retained after SQL redaction");
    assert!(native.statement.is_valid());
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
fn complete_native_prefix_supplies_only_its_exact_statement_and_redacts_private_values() {
    let first = "SELECT N'😀'; ";
    let selected = "SELECT 9264 AS customer_id, N'grant-private-literal' AS note";
    let batch = format!("{first}{selected}; SELECT 999 AS unrelated");
    let start = first.encode_utf16().count() * 2;
    let end = start + (selected.encode_utf16().count() - 1) * 2;
    let native = fact(&[
        ("text_prefix", Val::Text(batch.clone())),
        ("source_text_bytes", Val::Int(batch.encode_utf16().count() as i64 * 2)),
        ("statement_start_offset", Val::Int(start as i64)),
        ("statement_end_offset", Val::Int(end as i64)),
    ]);
    let DefinitionPayload::Available { text, .. } = &native.statement else { panic!("original complete statement") };
    assert!(text.contains("customer_id") && text.contains("note"));
    assert!(!text.contains("9264") && !text.contains("grant-private-literal"));
    assert!(!text.contains("😀") && !text.contains("unrelated"));
    assert_eq!(native.context.identity.statement_start_bytes as usize, start);
    assert_eq!(native.context.identity.statement_end_bytes as usize, end);
}

#[test]
fn missing_truncated_and_inconsistent_prefixes_withhold_sql_without_erasing_positive_grant_rows() {
    for fields in [
        vec![("text_prefix", Val::Null)],
        vec![("text_complete", Val::Int(0))],
        vec![("source_text_bytes", Val::Int(1000))],
        vec![("statement_start_offset", Val::Int(1000))],
        vec![("statement_end_offset", Val::Int(1000))],
    ] {
        let native = fact(&fields);
        assert!(!native.statement.is_available());
        assert_eq!(native.total_grant_kibibytes.as_deref(), Some("20"));
        assert_eq!(native.executions.as_deref(), Some("2"));
    }
}

#[tokio::test]
async fn selected_workload_reads_zero_grants_through_the_same_native_decoder_and_parameterizes_hashes() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![layout_row(true, &COLUMNS)]),
        Ok(vec![replace(&row(), &[("total_grant_kb", Val::Int(0)), ("total_used_grant_kb", Val::Int(0))])]),
    ]);
    let selected = vec!["0x0123456789ABCDEF".to_string()];
    let observations = application(&mut reader, &selected).await.unwrap().unwrap();
    assert_eq!(observations[0].total_grant_kibibytes.as_deref(), Some("0"));
    assert_eq!(observations[0].total_used_kibibytes.as_deref(), Some("0"));
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
    let last = replace(&row(), &[("query_hash", Val::Text(hashes[200].clone())), ("plan_generation_num", Val::Int(2))]);
    let mut reader = FakeRead::new(vec![Ok(vec![layout_row(true, &COLUMNS)]), Ok(vec![first]), Ok(vec![last])]);
    let result = application(&mut reader, &hashes).await.unwrap().unwrap();
    assert_eq!(result.len(), 2);
    assert!(result.iter().any(|grant| grant.context.query_hash.as_deref() == Some(hashes[200].as_str())));
    assert_eq!(reader.calls.len(), 3);
    let Parameter::Text(second) = &reader.calls[2].parameters[1] else { panic!("XML selection") };
    assert_eq!(second.matches("<h ").count(), 1, "duplicate native hashes must not be collected in later batches");
}

#[test]
fn zero_native_handles_keep_unqualified_grant_counters_unknown() {
    let native = fact(&[("plan_handle", Val::Bytes(vec![0; 64]))]);
    assert!(native.executions.is_none());
    assert!(native.total_grant_kibibytes.is_none() && native.total_used_kibibytes.is_none());
    assert!(!native.statement.is_available());
}

fn scoped_reader(rows: Vec<Rec>, before: Rec, after: Rec) -> FakeRead {
    FakeRead::new(vec![
        Ok(vec![platform()]),
        Ok(vec![before]),
        Ok(vec![layout_row(true, &COLUMNS)]),
        Ok(rows),
        Ok(vec![after]),
    ])
}

#[tokio::test]
async fn exact_cache_entries_remain_separate_with_their_native_owner_and_original_counters() {
    let first = row();
    let second =
        replace(&row(), &[("plan_generation_num", Val::Int(2)), ("total_grant_kb", Val::Int(9007199254740993))]);
    let mut reader =
        scoped_reader(vec![first, second], authority("2020-01-01T06:02:00Z"), authority("2020-01-01T06:02:01Z"));
    let hashes = vec!["0x0123456789ABCDEF".into()];
    let snapshot = collect_from(&mut reader, &hashes, "fixture.invalid", 1433).await.unwrap();
    assert!(snapshot.is_valid());
    assert!(snapshot.description.partition.is_qualified());
    assert_eq!(snapshot.observations.len(), 2, "one hash is not one native cache incarnation");
    assert_ne!(
        snapshot.observations[0].context.identity.key().unwrap(),
        snapshot.observations[1].context.identity.key().unwrap()
    );
    assert_eq!(snapshot.observations[0].total_grant_kibibytes.as_deref(), Some("20"));
    assert_eq!(snapshot.observations[1].total_grant_kibibytes.as_deref(), Some("9007199254740993"));
    let coverage = &snapshot.description.observation.snapshot().unwrap().sources[0];
    assert_eq!(
        coverage.state,
        culprit_core::SourceState::Partial,
        "hash-selected rows are not a complete cache inventory"
    );
    assert!(!coverage.complete_scope);
    assert_eq!(reader.calls.len(), 5);
    assert_eq!(reader.closed, 1);
    assert!(reader.calls.iter().all(
        |call| !call.sql.contains("sys.dm_exec_query_plan") && !call.sql.contains("sys.query_store_runtime_stats")
    ));
}

#[tokio::test]
async fn equal_native_clocks_do_not_make_independent_reads_the_same_capture() {
    let mut captures = Vec::new();
    let hashes = vec!["0x0123456789ABCDEF".into()];
    for _ in 0..2 {
        let mut reader =
            scoped_reader(vec![row()], authority("2020-01-01T06:02:00Z"), authority("2020-01-01T06:02:00Z"));
        captures.push(collect_from(&mut reader, &hashes, "fixture.invalid", 1433).await.unwrap());
    }
    assert_eq!(captures[0].description, captures[1].description);
    assert_eq!(captures[0].observations, captures[1].observations);
    assert_ne!(captures[0].capture, captures[1].capture);
    assert_ne!(captures[0].key().unwrap(), captures[1].key().unwrap());
}

#[tokio::test]
async fn no_selected_cache_entries_remains_partial_without_claiming_an_empty_cache() {
    let mut reader = scoped_reader(vec![], authority("2020-01-01T06:02:00Z"), authority("2020-01-01T06:02:01Z"));
    let hashes = vec!["0x0123456789ABCDEF".into()];
    let snapshot = collect_from(&mut reader, &hashes, "fixture.invalid", 1433).await.unwrap();
    assert!(snapshot.is_valid());
    assert!(snapshot.observations.is_empty());
    let coverage = &snapshot.description.observation.snapshot().unwrap().sources[0];
    assert_eq!(coverage.state, culprit_core::SourceState::Partial);
    assert_eq!(coverage.records, 0);
    assert!(!coverage.complete_scope);
    assert_eq!(reader.calls.len(), 5);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn selected_grants_cannot_replace_the_read_budget_to_complete_a_final_owner_check() {
    let mut reader = scoped_reader(vec![row()], authority("2020-01-01T06:02:00Z"), authority("2020-01-01T06:02:01Z"))
        .with_query_limit(4);
    let hashes = vec!["0x0123456789ABCDEF".into()];
    let error = collect_from(&mut reader, &hashes, "fixture.invalid", 1433).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::QueryLimit);
    assert_eq!(reader.calls.len(), 4);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn missing_member_keeps_a_partial_raw_point_but_source_change_and_unconfirmed_close_publish_nothing() {
    let unknown = replace(&authority("2020-01-01T06:02:00Z"), &[("replica_id", Val::Null)]);
    let mut reader = scoped_reader(vec![row()], unknown.clone(), unknown);
    let hashes = vec!["0x0123456789ABCDEF".into()];
    let snapshot = collect_from(&mut reader, &hashes, "fixture.invalid", 1433).await.unwrap();
    assert!(!snapshot.description.partition.is_qualified());
    assert_eq!(snapshot.observations.len(), 1);
    assert_eq!(snapshot.observations[0].total_grant_kibibytes.as_deref(), Some("20"));
    assert_eq!(
        snapshot.description.observation.snapshot().unwrap().sources[0].state,
        culprit_core::SourceState::Partial
    );
    for (name, value) in [
        ("replica_id", Val::Text("11111111-2222-3333-4444-555555555555".into())),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("local_started_ticks", Val::Int(200)),
    ] {
        let after = replace(&authority("2020-01-01T06:02:01Z"), &[(name, value)]);
        let mut reader = scoped_reader(vec![row()], authority("2020-01-01T06:02:00Z"), after);
        let error = collect_from(&mut reader, &hashes, "fixture.invalid", 1433).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{name}");
        assert_eq!(reader.closed, 1);
        assert_eq!(reader.calls.len(), 5);
    }
    let mut reader = scoped_reader(vec![row()], authority("2020-01-01T06:02:00Z"), authority("2020-01-01T06:02:01Z"));
    reader.shutdown = SourceShutdown::Unconfirmed;
    let error = collect_from(&mut reader, &hashes, "fixture.invalid", 1433).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
}

#[tokio::test]
async fn duplicate_entries_and_terminal_native_stops_do_not_trigger_more_sql() {
    let hashes = vec!["0x0123456789ABCDEF".into()];
    let mut duplicate = FakeRead::new(vec![Ok(vec![layout_row(true, &COLUMNS)]), Ok(vec![row(), row()])]);
    assert!(application(&mut duplicate, &hashes).await.is_err());
    assert_eq!(duplicate.calls.len(), 2);
    let mut stopped = FakeRead::new(vec![Err(crate::read::ReadError::Stopped(CollectionStop::Cancelled))]);
    let error = collect_from(&mut stopped, &hashes, "fixture.invalid", 1433).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::Cancelled);
    assert_eq!(stopped.calls.len(), 1);
    assert_eq!(stopped.closed, 1);
    let mut invalid = FakeRead::new(vec![]);
    let bad = vec!["0x0);DROP TABLE x;--".into()];
    assert!(collect_from(&mut invalid, &bad, "fixture.invalid", 1433).await.is_err());
    assert!(invalid.calls.is_empty(), "invalid selectors cannot reach owner discovery or data SQL");
    assert_eq!(invalid.closed, 1);
}

#[tokio::test]
async fn partial_native_column_support_remains_readable_for_selected_hashes_without_cache_ranking() {
    let subset = ["last_grant_kb"];
    let mut selected = FakeRead::new(vec![
        Ok(vec![layout_row(true, &subset)]),
        Ok(vec![replace(
            &row(),
            &[
                ("total_grant_kb", Val::Null),
                ("total_used_grant_kb", Val::Null),
                ("last_used_grant_kb", Val::Null),
                ("min_grant_kb", Val::Null),
                ("min_used_grant_kb", Val::Null),
                ("max_grant_kb", Val::Null),
                ("max_used_grant_kb", Val::Null),
            ],
        )]),
    ]);
    let hashes = vec!["0x0123456789ABCDEF".into()];
    let rows = application(&mut selected, &hashes).await.unwrap().unwrap();
    assert_eq!(rows[0].last_grant_kibibytes.as_deref(), Some("12"));
    assert_eq!(rows[0].total_grant_kibibytes, None);
    assert_eq!(selected.calls.len(), 2);
    assert!(selected.calls.iter().all(|call| !call.sql.contains("GROUP BY") && !call.sql.contains("ORDER BY")));
}

#[tokio::test]
async fn selected_native_totals_need_no_cache_aggregate_or_ranking() {
    let columns = ["total_grant_kb", "total_used_grant_kb"];
    let native = replace(
        &row(),
        &[
            ("last_grant_kb", Val::Null),
            ("last_used_grant_kb", Val::Null),
            ("min_grant_kb", Val::Null),
            ("min_used_grant_kb", Val::Null),
            ("max_grant_kb", Val::Null),
            ("max_used_grant_kb", Val::Null),
        ],
    );
    let mut selected = FakeRead::new(vec![Ok(vec![layout_row(true, &columns)]), Ok(vec![native])]);
    let hashes = vec!["0x0123456789ABCDEF".into()];
    let rows = application(&mut selected, &hashes).await.unwrap().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].total_grant_kibibytes.as_deref(), Some("20"));
    assert_eq!(rows[0].total_used_kibibytes.as_deref(), Some("10"));
    assert_eq!(rows[0].max_grant_kibibytes, None);
    assert_eq!(selected.calls.len(), 2);
    assert!(selected.calls[1].sql.contains("CAST(NULL AS bigint) AS max_grant_kb"));
    assert!(!selected.calls[1].sql.contains("GROUP BY") && !selected.calls[1].sql.contains("ORDER BY"));
}

#[tokio::test]
async fn a_scan_without_workload_selectors_keeps_a_source_gap_without_opening_cache_discovery() {
    let provider = crate::MssqlProvider {
        endpoint_host: "no-database.invalid".into(),
        endpoint_port: 1,
        environment: culprit_core::Environment::Development,
        pool: std::sync::Arc::new(crate::conn::Pool::new(
            crate::test_support::config(1, "unopened", "unused", "culprit"),
            None,
        )),
    };
    let probe = crate::probe::Probe {
        version: "16.0.0.0".into(),
        major: 16,
        edition: "fixture".into(),
        engine_edition: 3,
        database: "unopened".into(),
        data_bytes: None,
        can_view_database_state: true,
        can_view_server_state: true,
        can_showplan: true,
        can_view_definition: true,
        login: "fixture".into(),
        user: "fixture".into(),
        started_at: None,
        query_store: None,
        rds: false,
    };
    let mut coverage = culprit_core::Coverage::default();
    assert!(collect_application(&provider, &probe, &mut coverage, &[]).await.is_none());
    let outcomes = coverage.outcomes();
    let [outcome] = outcomes.as_slice() else { panic!("one explicit native-source gap") };
    assert_eq!(outcome.source, crate::sources::GRANTS.name);
    assert_eq!(outcome.state, culprit_core::SourceState::Unavailable);
    assert_eq!(outcome.reason.as_deref(), Some(culprit_core::t!("provider.mssql.grants.no_selectors").as_str()));
    assert_eq!(outcome.reason_message, Some(culprit_core::message!("provider.mssql.grants.no_selectors")));
    assert_eq!(provider.pool.source_shutdown(), SourceShutdown::Quiescent);

    let mut native = FakeRead::new(vec![]);
    let error = collect_from(&mut native, &[], "no-database.invalid", 1).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceUnavailable);
    assert!(native.calls.is_empty(), "missing selectors cannot reach source metadata or a cache read");
    assert_eq!(native.closed, 1);
}
