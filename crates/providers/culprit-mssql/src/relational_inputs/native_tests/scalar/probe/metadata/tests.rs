use super::*;
use crate::{
    relational_inputs::tests::binding_row,
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

const OBJECT: i64 = 1001;
const STATEMENT: &str = "SELECT COUNT_BIG(pad) FROM dbo.scalar_rows WHERE bucket=@bucket";
pub(in super::super) fn row() -> Rec {
    let prefix = "CREATE PROCEDURE dbo.scalar_probe @bucket int AS ";
    let header = format!("{prefix}{STATEMENT}");
    let native = replace(
        &crate::cached_definitions::tests::module_row(STATEMENT, crate::cached_definitions::tests::XML),
        &[
            ("text_prefix", Val::Text(header.clone())),
            ("source_text_bytes", Val::Int((header.encode_utf16().count() * 2) as i64)),
            ("statement_start_offset", Val::Int((prefix.encode_utf16().count() * 2) as i64)),
        ],
    );
    Rec::of(
        &native
            .columns()
            .map(|(name, value)| (name, value.clone()))
            .chain([(EXACT_SELECTION, Val::Int(1))])
            .collect::<Vec<_>>(),
    )
}

#[tokio::test]
async fn one_exact_module_header_and_existing_independent_binding_replace_all_database_plan_payloads() {
    let row = row();
    let original = cached_statements::context(&row).unwrap();
    let mut reader = FakeRead::new(vec![Ok(vec![row]), Ok(vec![binding_row(&original)])]);
    let selected = read(&mut reader, OBJECT, Some(&original)).await.unwrap();
    assert_eq!(selected.context, original);
    assert_eq!(selected.statement, STATEMENT);
    assert_eq!(selected.module_parameters.object_id, OBJECT as i32);
    assert!(selected.module_parameters.table_parameters.is_empty());
    assert!(selected.module_parameters.is_valid_for(&selected.context));
    assert_eq!(reader.calls.len(), 2);
    let call = &reader.calls[0];
    assert_eq!(call.limit, SELECTED_ROWS);
    assert_eq!(call.parameters.len(), 8);
    assert!(call.sql.contains("TOP(@P1)") && call.sql.contains("st.objectid=@P2 AND st.number=1"));
    assert!(call.sql.contains(cached_statements::DATABASE_PREDICATE));
    assert!(call.sql.contains("st.dbid=DB_ID()"));
    for field in [
        "plan_handle=CONVERT",
        "sql_handle=CONVERT",
        "statement_start_offset=@P5",
        "statement_end_offset=@P6",
        "plan_generation_num=@P7",
    ] {
        assert!(call.sql.contains(field), "{field}");
    }
    for forbidden in [
        "sys.dm_exec_text_query_plan",
        "sys.dm_exec_query_plan_stats",
        "sys.parameters",
        "current_type_schema",
        "sys.columns",
        "scalar_rows",
    ] {
        assert!(!call.sql.contains(forbidden), "{forbidden}");
    }
    assert!(reader.calls[1].sql.contains("sys.dm_exec_plan_attributes(selected.plan_handle)"));
}

#[tokio::test]
async fn absent_or_ambiguous_native_points_and_unverified_bindings_have_no_representative_fallback() {
    let row = row();
    let original = cached_statements::context(&row).unwrap();
    for rows in [vec![], vec![row.clone(), row.clone()]] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(read(&mut reader, OBJECT, Some(&original)).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
    let changed = replace(&binding_row(&original), &[("sql_handle", Val::Text("0x9999".into()))]);
    let mut reader = FakeRead::new(vec![Ok(vec![row]), Ok(vec![changed])]);
    let error = read(&mut reader, OBJECT, Some(&original)).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert_eq!(reader.calls.len(), 2);
}

#[tokio::test]
async fn full_creation_generation_handles_statement_database_and_module_ownership_are_rechecked() {
    let original_row = row();
    let original = cached_statements::context(&original_row).unwrap();
    for (field, value) in [
        ("creation_time", Val::Text("2026-11-21T12:00:00.003".into())),
        ("plan_generation_num", Val::Int(9)),
        ("plan_handle", Val::Text("0x9999".into())),
        ("sql_handle", Val::Text("0x9999".into())),
        ("statement_start_offset", Val::Int(0)),
        ("statement_end_offset", Val::Int(i64::from(original.identity.statement_start_bytes) + 2)),
        ("database_id", Val::Int(99)),
        ("query_hash", Val::Text("0x9999999999999999".into())),
        ("module_object_id", Val::Int(99)),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&original_row, &[(field, value)])])]);
        let error = read(&mut reader, OBJECT, Some(&original)).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert_eq!(reader.calls.len(), 1, "{field}");
    }
}

#[tokio::test]
async fn missing_partial_encrypted_or_foreign_native_headers_cannot_supply_module_or_statement_context() {
    for (field, value) in [
        ("text_complete", Val::Int(0)),
        ("text_prefix", Val::Null),
        ("encrypted", Val::Bool(true)),
        ("module_database_id", Val::Int(99)),
        ("module_number", Val::Int(2)),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[(field, value)])])]);
        assert!(read(&mut reader, OBJECT, None).await.is_err(), "{field}");
        assert_eq!(reader.calls.len(), 1, "{field}");
    }
}

#[tokio::test]
async fn cancellation_timeout_and_unconfirmed_native_reads_never_start_an_independent_binding_query() {
    for stop in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed] {
        let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        let error = read(&mut reader, OBJECT, None).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.calls.len(), 1);
    }
}

#[test]
fn exact_selector_binds_full_width_native_hex_and_signed_offsets_to_their_declared_positions() {
    let mut original = cached_statements::context(&row()).unwrap();
    original.identity.plan_handle = crate::value::hex(&[0xA5; 64]);
    original.identity.sql_handle = crate::value::hex(&[0x5A; 64]);
    original.identity.statement_start_bytes = 96;
    original.identity.statement_end_bytes = -1;
    original.identity.plan_generation = "9223372036854775807".into();
    let (sql, parameters) = query(OBJECT, Some(&original)).unwrap();
    assert_eq!(parameters.len(), 8);
    for (position, expected) in
        [(0, 2), (1, OBJECT), (4, 96), (5, -1), (6, i64::MAX), (7, i64::from(original.database_id))]
    {
        assert!(
            matches!(&parameters[position], Parameter::Integer(value) if *value == expected),
            "position {position}"
        );
    }
    assert!(matches!(&parameters[2], Parameter::Text(value) if value == &original.identity.plan_handle));
    assert!(matches!(&parameters[3], Parameter::Text(value) if value == &original.identity.sql_handle));
    assert!(sql.contains("OUTER APPLY(SELECT TOP(@P1)"));
    assert!(sql.contains("qs.plan_handle=CONVERT(varbinary(64),@P3,1)"));
    assert!(sql.contains("qs.sql_handle=CONVERT(varbinary(64),@P4,1)"));
    assert!(sql.contains("CAST(pa.value AS int)=@P8"));
    assert_eq!(sql.matches("sys.dm_exec_query_stats").count(), 1);
    assert!(!query(OBJECT, None).unwrap().0.contains("culprit_bind_"));
}

#[tokio::test]
async fn native_parameter_diagnostics_never_turn_a_missing_exact_selection_into_context_or_a_binding_read() {
    let original = cached_statements::context(&row()).unwrap();
    for value in [Val::Int(0), Val::Int(1), Val::Null] {
        let native = Rec::of(&[
            (EXACT_SELECTION, Val::Null),
            ("culprit_bind_database", value.clone()),
            ("culprit_bind_plan", value.clone()),
            ("culprit_bind_sql", value.clone()),
            ("culprit_bind_integers", value.clone()),
            ("culprit_bind_offsets", value),
        ]);
        let mut reader = FakeRead::new(vec![Ok(vec![native])]);
        let error = read(&mut reader, OBJECT, Some(&original)).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceUnavailable);
        assert_eq!(reader.calls.len(), 1);
        assert_eq!(reader.calls[0].limit, SELECTED_ROWS);
    }
}

#[test]
fn diagnostic_cache_comparison_exposes_only_exact_native_component_equality() {
    let original = cached_statements::context(&row()).unwrap();
    let exact = serde_json::to_value(CacheContextComparison::between(&original, &original)).unwrap();
    assert_eq!(exact.as_object().unwrap().len(), 9);
    assert!(exact.as_object().unwrap().values().all(|value| value.as_bool() == Some(true)));
    assert!(CacheContextComparison::between(&original, &original).require_original_match().is_ok());
    for (column, value, flag) in [
        ("database_id", Val::Int(99), "database_matches"),
        ("plan_handle", Val::Text("0x9999".into()), "plan_handle_matches"),
        ("sql_handle", Val::Text("0x9999".into()), "sql_handle_matches"),
        ("statement_start_offset", Val::Int(0), "statement_start_matches"),
        (
            "statement_end_offset",
            Val::Int(i64::from(original.identity.statement_start_bytes) + 2),
            "statement_end_matches",
        ),
        ("plan_generation_num", Val::Int(9), "plan_generation_matches"),
        ("creation_time", Val::Text("2026-11-21T12:00:00.003".into()), "creation_time_matches"),
        ("query_hash", Val::Text("0x9999999999999999".into()), "query_hash_matches"),
    ] {
        let current = cached_statements::context(&replace(&row(), &[(column, value)])).unwrap();
        let flags = serde_json::to_value(CacheContextComparison::between(&original, &current)).unwrap();
        assert_eq!(flags[flag], false, "{column}");
        assert_eq!(flags["original_incarnation_matches"], false, "{column}");
        let error = CacheContextComparison::between(&original, &current).require_original_match().unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{column}");
        for (key, value) in flags.as_object().unwrap() {
            assert!(value.is_boolean(), "{column}/{key}");
            if key != flag && key != "original_incarnation_matches" {
                assert_eq!(value.as_bool(), Some(true), "{column}/{key}");
            }
        }
    }
}

#[tokio::test]
async fn a_valid_current_cache_diagnostic_cannot_replace_the_original_incarnation_guard() {
    let original = cached_statements::context(&row()).unwrap();
    let changed = replace(&row(), &[("plan_generation_num", Val::Int(9))]);
    let current = cached_statements::context(&changed).unwrap();
    let mut diagnostic = FakeRead::new(vec![Ok(vec![changed.clone()]), Ok(vec![binding_row(&current)])]);
    let point = read(&mut diagnostic, OBJECT, None).await.unwrap();
    assert_eq!(diagnostic.calls.len(), 2);
    assert!(!CacheContextComparison::between(&original, &point.context).original_incarnation_matches);
    let error = CacheContextComparison::between(&original, &point.context).require_original_match().unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    let mut exact = FakeRead::new(vec![Ok(vec![changed])]);
    let error = read(&mut exact, OBJECT, Some(&original)).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert_eq!(exact.calls.len(), 1);
}

#[tokio::test]
async fn optional_exact_selection_distinguishes_completed_absence_from_an_unavailable_native_read() {
    let original = cached_statements::context(&row()).unwrap();
    let missing = missing_row();
    let mut io = FakeRead::new(vec![Ok(vec![missing.clone()])]);
    assert!(find(&mut io, OBJECT, Some(&original)).await.unwrap().is_none());
    assert_eq!(io.calls.len(), 1);
    for name in [
        "culprit_bind_database",
        "culprit_bind_plan",
        "culprit_bind_sql",
        "culprit_bind_integers",
        "culprit_bind_offsets",
    ] {
        for value in [Val::Int(0), Val::Null] {
            let mut io = FakeRead::new(vec![Ok(vec![replace(&missing, &[(name, value)])])]);
            assert!(find(&mut io, OBJECT, Some(&original)).await.is_err(), "{name}");
            assert_eq!(io.calls.len(), 1);
        }
    }
    let mut missing_witness = FakeRead::new(vec![Ok(vec![])]);
    assert!(find(&mut missing_witness, OBJECT, Some(&original)).await.is_err());
    assert_eq!(missing_witness.calls.len(), 1);
    for error in [
        ReadError::Stopped(CollectionStop::CancellationUnconfirmed),
        ReadError::Stopped(CollectionStop::TimedOut),
        ReadError::Stopped(CollectionStop::Cancelled),
        ReadError::Source(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "denied")),
    ] {
        let mut io = FakeRead::new(vec![Err(error)]);
        assert!(find(&mut io, OBJECT, Some(&original)).await.is_err());
        assert_eq!(io.calls.len(), 1);
    }
}

#[tokio::test]
async fn optional_exact_selection_keeps_binding_ambiguity_header_and_incarnation_refusals() {
    let original = cached_statements::context(&row()).unwrap();
    let mut matched = FakeRead::new(vec![Ok(vec![row()]), Ok(vec![binding_row(&original)])]);
    assert_eq!(find(&mut matched, OBJECT, Some(&original)).await.unwrap().unwrap().context, original);
    assert_eq!(matched.calls.len(), 2);
    for rows in [
        vec![row(), row()],
        vec![replace(&row(), &[("plan_generation_num", Val::Int(9))])],
        vec![replace(&row(), &[("text_complete", Val::Int(0))])],
    ] {
        let mut io = FakeRead::new(vec![Ok(rows)]);
        assert!(find(&mut io, OBJECT, Some(&original)).await.is_err());
        assert_eq!(io.calls.len(), 1);
    }
    let mut bad_binding = FakeRead::new(vec![
        Ok(vec![row()]),
        Ok(vec![replace(&binding_row(&original), &[("sql_handle", Val::Text("0x9999".into()))])]),
    ]);
    assert!(find(&mut bad_binding, OBJECT, Some(&original)).await.is_err());
    assert_eq!(bad_binding.calls.len(), 2);
}

pub(in super::super) fn missing_row() -> Rec {
    Rec::of(&[
        (EXACT_SELECTION, Val::Null),
        ("culprit_bind_database", Val::Int(1)),
        ("culprit_bind_plan", Val::Int(1)),
        ("culprit_bind_sql", Val::Int(1)),
        ("culprit_bind_integers", Val::Int(1)),
        ("culprit_bind_offsets", Val::Int(1)),
    ])
}

pub(in super::super) fn predicate_row(row: Rec) -> Rec {
    let flags = ORIGINAL_PREDICATES
        .iter()
        .map(|(name, _)| (format!("culprit_predicate_{name}"), Val::Int(1)))
        .collect::<Vec<_>>();
    Rec::of(
        &row.columns()
            .map(|(name, value)| (name, value.clone()))
            .chain(flags.iter().map(|(name, value)| (name.as_str(), value.clone())))
            .collect::<Vec<_>>(),
    )
}

#[test]
fn conditional_predicate_point_uses_the_exact_selectors_eight_bindings_and_one_module_population() {
    let original = cached_statements::context(&row()).unwrap();
    let (_, exact_parameters) = query(OBJECT, Some(&original)).unwrap();
    let (sql, diagnostic_parameters) = selected_query(OBJECT, Selection::Predicates(&original)).unwrap();
    assert_eq!(diagnostic_parameters.len(), 8);
    for (exact, diagnostic) in exact_parameters.iter().zip(&diagnostic_parameters) {
        match (exact, diagnostic) {
            (Parameter::Integer(left), Parameter::Integer(right)) => assert_eq!(left, right),
            (Parameter::Text(left), Parameter::Text(right)) => assert_eq!(left, right),
            _ => panic!("the same bound native types are retained"),
        }
    }
    assert_eq!(sql.matches("sys.dm_exec_query_stats").count(), 1);
    assert!(sql.contains(cached_statements::DATABASE_PREDICATE));
    assert!(sql.ends_with("AND st.dbid=DB_ID() AND st.objectid=@P2 AND st.number=1  OPTION(MAXDOP 1)"));
    for (name, predicate) in ORIGINAL_PREDICATES {
        assert!(sql.contains(&format!(
            "CASE WHEN {predicate} THEN 1 WHEN NOT({predicate}) THEN 0 END AS culprit_predicate_{name}"
        )));
    }
    assert!(!sql.contains(EXACT_SELECTION));
}

#[tokio::test]
async fn original_native_point_retains_full_context_and_each_predicate_without_lending_statement_sql() {
    let original = cached_statements::context(&row()).unwrap();
    for false_flag in [None].into_iter().chain(ORIGINAL_PREDICATES.iter().map(|(name, _)| Some(*name))) {
        let mut native = predicate_row(row());
        if let Some(name) = false_flag {
            native = replace(&native, &[(&format!("culprit_predicate_{name}"), Val::Int(0))]);
        }
        let mut reader = FakeRead::new(vec![Ok(vec![native]), Ok(vec![binding_row(&original)])]);
        let point = compare_predicates(&mut reader, OBJECT, &original).await.unwrap().unwrap();
        assert_eq!(point.context, original);
        assert_eq!(point.original_binding_verified, Some(true));
        assert!(point.comparison.original_incarnation_matches);
        assert_eq!(point.native_predicates.len(), ORIGINAL_PREDICATES.len());
        for (name, value) in &point.native_predicates {
            assert_eq!(*value, false_flag != Some(name.as_str()));
        }
        assert_eq!(reader.queries(), 2);
        assert!(!serde_json::to_string(&point).unwrap().contains(STATEMENT));
    }
}

#[tokio::test]
async fn changed_current_native_incarnation_has_context_and_flags_but_no_original_binding_dispatch() {
    let original = cached_statements::context(&row()).unwrap();
    let current = replace(
        &predicate_row(row()),
        &[("plan_generation_num", Val::Int(9)), ("culprit_predicate_plan_generation_matches", Val::Int(0))],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![current])]);
    let point = compare_predicates(&mut reader, OBJECT, &original).await.unwrap().unwrap();
    assert_eq!(point.context.identity.plan_generation, "9");
    assert!(!point.comparison.original_incarnation_matches);
    assert_eq!(point.native_predicates["plan_generation_matches"], false);
    assert_eq!(point.original_binding_verified, None);
    assert_eq!(reader.queries(), 1);
}

#[tokio::test]
async fn partial_ambiguous_unknown_or_failed_predicate_points_never_start_binding_work() {
    let original = cached_statements::context(&row()).unwrap();
    let native = predicate_row(row());
    let mut absent = FakeRead::new(vec![Ok(vec![])]);
    assert!(compare_predicates(&mut absent, OBJECT, &original).await.unwrap().is_none());
    for response in [
        Ok(vec![native.clone(), native.clone()]),
        Ok(vec![replace(&native, &[("text_complete", Val::Int(0))])]),
        Ok(vec![replace(&native, &[("module_object_id", Val::Int(99))])]),
        Ok(vec![replace(&native, &[("culprit_predicate_plan_handle_matches", Val::Null)])]),
        Err(ReadError::Stopped(CollectionStop::Cancelled)),
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)),
        Err(ReadError::Source(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            "PRIVATE_DIAGNOSTIC_ERROR",
        ))),
    ] {
        let mut reader = FakeRead::new(vec![response]);
        assert!(compare_predicates(&mut reader, OBJECT, &original).await.is_err());
        assert_eq!(reader.queries(), 1);
    }
}
