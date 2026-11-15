use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};

pub(crate) const AT: &str = "2020-01-01T06:00:00Z";

pub(crate) fn statement_row(start: i64, end: i64) -> Rec {
    let batch = "SELECT 1;SELECT 2";
    let row = replace(
        &crate::grants::tests::row(),
        &[
            ("statement_start_offset", Val::Int(start)),
            ("statement_end_offset", Val::Int(end)),
            ("source_text_bytes", Val::Int(batch.len() as i64 * 2)),
            ("text_prefix", Val::Text(batch.into())),
        ],
    );
    let mut columns = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    columns.extend([("row_kind", Val::Text("statement".into())), ("observed_at", Val::Text(AT.into()))]);
    Rec::of(&columns)
}

pub(crate) fn native_rows() -> Vec<Rec> {
    let context = cached_statements::context(&statement_row(0, 14)).unwrap();
    vec![
        Rec::of(&[("row_kind", Val::Text("clock".into())), ("observed_at", Val::Text(AT.into()))]),
        statement_row(0, 14),
        statement_row(18, -1),
        Rec::of(&[
            ("row_kind", Val::Text("entry".into())),
            ("observed_at", Val::Text(AT.into())),
            ("plan_handle", Val::Text(context.identity.plan_handle.clone())),
            ("sql_handle", Val::Text(context.identity.sql_handle)),
            ("size_in_bytes", Val::Int(32768)),
            ("cacheobjtype", Val::Text("Compiled Plan".into())),
            ("objtype", Val::Text("Proc".into())),
            ("usecounts", Val::Int(1)),
            ("refcounts", Val::Int(2)),
        ]),
        Rec::of(&[
            ("row_kind", Val::Text("key".into())),
            ("plan_handle", Val::Text(context.identity.plan_handle)),
            ("attribute_name", Val::Text("set_options".into())),
            ("native_type", Val::Text("int".into())),
            ("attribute_value", Val::Text("251".into())),
        ]),
    ]
}

pub(crate) async fn focus() -> CacheEntryMemoryFocus {
    let mut reader =
        FakeRead::new(vec![Ok(vec![crate::grants::tests::platform()]), Ok(vec![crate::grants::tests::authority(AT)])]);
    let owner = crate::local_source::Context::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
    CacheEntryMemoryFocus {
        context: NativeReadContext {
            target: owner.activity.database.clone().into(),
            partition: owner.source_partition().unwrap(),
            observed_from: AT.into(),
            observed_until: AT.into(),
            database_writable: Some(true),
        },
        statements: vec![
            cached_statements::context(&statement_row(0, 14)).unwrap(),
            cached_statements::context(&statement_row(18, -1)).unwrap(),
        ],
    }
}

#[tokio::test]
async fn the_selected_multi_statement_entry_is_counted_once_and_cache_lookups_are_not_executions() {
    let focus = focus().await;
    let mut reader = FakeRead::new(vec![Ok(native_rows())]);
    let point = read(&mut reader, &focus, &CollectionBudget::default()).await.unwrap();
    assert_eq!(point.entries.len(), 1);
    let entry = &point.entries[0];
    assert_eq!(entry.size_bytes, "32768");
    assert_eq!(entry.lookup_count, "1");
    assert_eq!(entry.statements.len(), 2);
    assert!(entry.statements.iter().all(|statement| statement.statement.is_available()));
    assert_eq!(reader.calls.len(), 1);
    let call = &reader.calls[0];
    assert!(call.sql.contains("sys.dm_exec_plan_attributes(selected.plan_handle)"));
    assert!(call.sql.contains("cp.plan_handle=selected.plan_handle"));
    assert!(call.sql.contains("qs.plan_generation_num=selected.generation"));
    assert!(call.sql.contains("qs.sql_handle=selected.sql_handle"));
    assert!(call.sql.contains("CONVERT(int,db.native_value)=DB_ID()"));
    assert!(call.sql.contains("OPTION(MAXDOP 1,LOOP JOIN)"));
    for forbidden in ["sys.dm_exec_query_plan", "sys.dm_exec_text_query_plan", "GROUP BY", "ORDER BY", "SUM(", "EXEC("]
    {
        assert!(!call.sql.contains(forbidden), "{forbidden}");
    }
}

#[tokio::test]
async fn fractional_creation_selectors_keep_native_identity_and_use_datetime_comparisons() {
    let original = focus().await;
    for fraction in ["003", "007", "333"] {
        let created = format!("2019-12-31T22:00:00.{fraction}");
        let mut rows = native_rows();
        for row in &mut rows[1..3] {
            *row = replace(row, &[("creation_time", Val::Text(created.clone()))]);
        }
        let mut focus = original.clone();
        focus.statements = rows[1..3].iter().map(|row| cached_statements::context(row).unwrap()).collect();
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        let point = read(&mut reader, &focus, &CollectionBudget::default()).await.unwrap();
        assert_eq!(point.entries[0].statements.len(), 2);
        for (observed, selected) in point.entries[0].statements.iter().zip(&focus.statements) {
            assert_eq!(observed.context.identity, selected.identity);
        }
        let call = &reader.calls[0];
        let Parameter::Text(selection) = &call.parameters[0] else { panic!() };
        assert!(selection.contains(&focus.statements[0].identity.created_without_offset));
        assert!(call.sql.contains("created datetime,generation"));
        assert!(call.sql.contains("CONVERT(datetime,n.x.value('@created[1]'"));
        assert!(call.sql.contains("qs.creation_time=selected.created"));
    }
}

#[tokio::test]
async fn missing_or_duplicate_selectors_do_no_native_work() {
    let mut focus = focus().await;
    let mut reader = FakeRead::new(vec![]);
    focus.statements.clear();
    assert!(read(&mut reader, &focus, &CollectionBudget::default()).await.is_err());
    focus.statements.push(cached_statements::context(&statement_row(0, 14)).unwrap());
    focus.statements.push(focus.statements[0].clone());
    assert!(read(&mut reader, &focus, &CollectionBudget::default()).await.is_err());
    assert!(reader.calls.is_empty());
}

#[tokio::test]
async fn native_metric_changes_are_allowed_but_changed_keys_and_statement_incarnations_are_not() {
    let focus = focus().await;
    let initial = decode(native_rows(), &focus, &CollectionBudget::default()).unwrap();
    let mut volatile = native_rows();
    volatile[3] = replace(
        &volatile[3],
        &[("usecounts", Val::Int(100)), ("refcounts", Val::Int(8)), ("size_in_bytes", Val::Int(65536))],
    );
    let later = decode(volatile, &focus, &CollectionBudget::default()).unwrap();
    assert!(same_bindings(&initial, &later));
    let mut changed = native_rows();
    changed[4] = replace(&changed[4], &[("attribute_value", Val::Text("252".into()))]);
    assert!(!same_bindings(&initial, &decode(changed, &focus, &CollectionBudget::default()).unwrap()));
    let mut unknown = native_rows();
    unknown[4] = replace(&unknown[4], &[("attribute_value", Val::Null)]);
    assert!(!same_bindings(&initial, &decode(unknown, &focus, &CollectionBudget::default()).unwrap()));
    for field in ["plan_generation_num", "statement_start_offset"] {
        let mut changed = native_rows();
        changed[1] = replace(&changed[1], &[(field, Val::Int(9))]);
        assert!(decode(changed, &focus, &CollectionBudget::default()).is_err(), "{field}");
    }
}

#[tokio::test]
async fn missing_keys_clock_conflicts_duplicates_and_unrequested_entries_are_explicit_failures() {
    let focus = focus().await;
    let mut duplicate = native_rows();
    duplicate.push(duplicate[3].clone());
    assert!(decode(duplicate, &focus, &CollectionBudget::default()).is_err());
    let mut wrong = native_rows();
    wrong[3] = replace(&wrong[3], &[("plan_handle", Val::Text("0x0103".into()))]);
    assert!(decode(wrong, &focus, &CollectionBudget::default()).is_err());
    let mut changed = native_rows();
    changed[3] = replace(&changed[3], &[("observed_at", Val::Text("2020-01-01T06:00:01Z".into()))]);
    assert!(decode(changed, &focus, &CollectionBudget::default()).is_err());
    let mut missing = native_rows();
    missing.pop();
    let before = decode(missing.clone(), &focus, &CollectionBudget::default()).unwrap();
    let after = decode(missing, &focus, &CollectionBudget::default()).unwrap();
    assert!(!same_bindings(&before, &after), "missing keys never prove stable cache-key context");
}

#[tokio::test]
async fn available_sql_never_expands_beyond_the_retained_native_prefix_or_caller_payload_limit() {
    let focus = focus().await;
    let mut rows = native_rows();
    rows[1] = replace(&rows[1], &[("text_complete", Val::Int(0))]);
    let point = decode(rows, &focus, &CollectionBudget::default()).unwrap();
    assert!(matches!(
        point.entries[0].statements[0].statement,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }
    ));
    assert!(point.entries[0].statements[1].statement.is_available());

    let mut rows = native_rows();
    let batch = format!("SELECT {}field FROM dbo.orders", "field,".repeat(200));
    let bytes = (batch.encode_utf16().count() * 2) as i64;
    let mut selected = focus.clone();
    for context in &mut selected.statements {
        context.source_text_bytes = Some(bytes.to_string());
    }
    for row in &mut rows[1..=2] {
        *row = replace(row, &[("source_text_bytes", Val::Int(bytes)), ("text_prefix", Val::Text(batch.clone()))]);
    }
    let budget = CollectionBudget { max_sql_bytes: 1024, ..CollectionBudget::default() };
    let limited = decode(rows, &selected, &budget).unwrap();
    assert!(limited.entries[0].statements.iter().all(|statement| matches!(
        statement.statement,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
    )));
}

#[test]
fn the_source_envelope_intersects_every_changed_limit_without_replacing_caller_limits() {
    let requested = CollectionBudget {
        max_queries: 2,
        statement_timeout_ms: 100,
        total_timeout_ms: 200,
        page_rows: 1,
        max_snapshot_rows: 1,
        ..CollectionBudget::default()
    };
    let bounded = bounded_budget(&requested);
    assert_eq!(bounded.max_queries, 2);
    assert_eq!(bounded.statement_timeout_ms, 100);
    assert_eq!(bounded.total_timeout_ms, 200);
    assert_eq!(bounded.page_rows, 1);
    assert_eq!(bounded.max_snapshot_rows, 1);
    assert_eq!(bounded.page_bytes, requested.page_bytes);
    let upper = bounded_budget(&CollectionBudget::upper_limits());
    assert_eq!(upper.max_queries, MAX_QUERIES);
    assert_eq!(upper.statement_timeout_ms, 1_000);
    assert_eq!(upper.total_timeout_ms, 5_000);
    assert!(upper.validate().is_ok());
}
