use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
fn layout() -> Rec {
    Rec::of(&[
        ("metadata_visibility", Val::Int(1)),
        ("last_actual", Val::Int(1)),
        ("live_xml", Val::Int(1)),
        ("profiles", Val::Int(1)),
    ])
}
fn row() -> Rec {
    let mut columns = crate::grants::tests::row()
        .columns()
        .map(|(name, value)| (name.to_string(), value.clone()))
        .collect::<Vec<_>>();
    columns.extend([
        (String::from("plan_bytes"), Val::Int(2)),
        ("query_plan".into(), Val::Text("x".into())),
        ("encrypted".into(), Val::Bool(false)),
        ("module_database_id".into(), Val::Null),
        ("module_object_id".into(), Val::Null),
        ("module_number".into(), Val::Null),
        ("last_start_without_offset".into(), Val::Text("2020-01-01T08:00:00.003".into())),
    ]);
    let identities = replace(
        &crate::query_store::tests::summary_row(),
        &[("query_id", Val::Int(7)), ("definition_plan_id", Val::Int(9))],
    );
    for name in [
        "query_id",
        "context_settings_id",
        "object_id",
        "query_hash",
        "query_initial_compile_at",
        "definition_plan_id",
        "plan_hash",
        "plan_initial_compile_at",
    ] {
        let value = identities.columns().find(|(column, _)| *column == name).unwrap().1.clone();
        columns.push((format!("query_store_{name}"), value));
    }
    Rec::of(&columns.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>())
}
fn request(plan_id: Option<&str>) -> RetainedActualPlanRequest {
    RetainedActualPlanRequest {
        query_id: "7".into(),
        plan_id: plan_id.map(str::to_owned),
        expected_target: None,
        expected_query: None,
        expected_plan: None,
    }
}
#[tokio::test]
async fn retained_lookup_binds_selectors_and_requires_the_current_compilation_database() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![layout()]),
        Ok(vec![Rec::of(&[("value", Val::Int(1))])]),
        Ok(vec![row()]),
        Ok(vec![crate::relational_inputs::tests::binding_row(&cached_statements::context(&row()).unwrap())]),
    ]);
    let result = read(&mut reader, &request(Some("9")), 1024).await.unwrap();
    let plan = result.plan.unwrap();
    assert!(plan.payload.text().is_some());
    assert_eq!(plan.last_start_without_offset.as_deref(), Some("2020-01-01T08:00:00.0030000"));
    let call = &reader.calls[2];
    assert!(call.sql.contains(cached_statements::DATABASE_PREDICATE));
    assert!(call.sql.contains("q.query_id=@P1"));
    assert!(call.sql.contains("p.plan_id=@P2"));
    assert!(matches!(call.parameters[0], Parameter::Integer(7)));
    assert!(matches!(call.parameters[1], Parameter::Integer(9)));
    assert_eq!(reader.calls.len(), 4);
    assert!(reader.calls[3].sql.contains("sys.dm_exec_plan_attributes(selected.plan_handle)"));
}
#[tokio::test]
async fn failed_setting_reads_never_continue_with_a_plan_lookup() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![layout()]),
        Err(ReadError::Stopped(culprit_core::telemetry::CollectionStop::TimedOut)),
    ]);
    let error = read(&mut reader, &request(None), 1024).await.err().unwrap();
    assert_eq!(
        culprit_core::telemetry::CollectionStop::from_error(&error),
        culprit_core::telemetry::CollectionStop::TimedOut
    );
    assert_eq!(reader.calls.len(), 2);
}
#[tokio::test]
async fn oversized_retained_xml_is_unavailable_without_truncation() {
    let oversized = replace(&row(), &[("plan_bytes", Val::Int(2048)), ("query_plan", Val::Null)]);
    let mut reader =
        FakeRead::new(vec![Ok(vec![layout()]), Ok(vec![Rec::of(&[("value", Val::Int(1))])]), Ok(vec![oversized])]);
    let result = read(&mut reader, &request(None), 1024).await.unwrap();
    assert!(matches!(
        result.plan.unwrap().payload,
        Payload::Unavailable(culprit_core::telemetry::PayloadUnavailable::Oversized)
    ));
}

#[tokio::test]
async fn reused_or_missing_plan_binding_withholds_xml_and_preserves_the_earlier_native_context_and_counters() {
    let original = decode(&row(), 1024, MAX_STATEMENT_BYTES).unwrap();
    let binding = crate::relational_inputs::tests::binding_row(&original.context);
    for sql_handle in [Val::Text("0x9999".into()), Val::Null] {
        let mut reader = FakeRead::new(vec![
            Ok(vec![layout()]),
            Ok(vec![Rec::of(&[("value", Val::Int(1))])]),
            Ok(vec![row()]),
            Ok(vec![replace(&binding, &[("sql_handle", sql_handle)])]),
        ]);
        let observed = read(&mut reader, &request(Some("9")), 1024).await.unwrap();
        assert_eq!(observed.setting_on, Some(true), "a changed binding is not evidence to turn statistics on");
        let observed = observed.plan.unwrap();
        assert!(matches!(observed.payload, Payload::Unavailable(PayloadUnavailable::SourceUnavailable)));
        assert_eq!(observed.context, original.context);
        assert_eq!(
            observed.executions, original.executions,
            "module binding never replaces the earlier counter population"
        );
        assert_eq!(observed.last_start_without_offset, original.last_start_without_offset);
        assert_eq!(reader.calls.len(), 4);
    }
}

#[tokio::test]
async fn original_tvp_metadata_travels_with_the_retained_read_and_is_withheld_when_the_binding_changes() {
    let native = crate::retained_definitions::tests::tvp_row();
    let mut columns = native.columns().map(|(name, value)| (name.to_string(), value.clone())).collect::<Vec<_>>();
    columns.extend(
        row()
            .columns()
            .filter(|(name, _)| name.starts_with("query_store_"))
            .map(|(name, value)| (name.to_string(), value.clone())),
    );
    let native = Rec::of(&columns.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>());
    let original = cached_statements::context(&native).unwrap();
    let binding = crate::relational_inputs::tests::binding_row(&original);
    for changed in [false, true] {
        let observed_binding =
            if changed { replace(&binding, &[("sql_handle", Val::Text("0x9999".into()))]) } else { binding.clone() };
        let mut reader = FakeRead::new(vec![
            Ok(vec![layout()]),
            Ok(vec![Rec::of(&[("value", Val::Int(1))])]),
            Ok(vec![native.clone()]),
            Ok(vec![observed_binding]),
        ]);
        let retained = read(&mut reader, &request(Some("9")), 4096).await.unwrap().plan.unwrap();
        assert_eq!(reader.calls.len(), 4, "module and statement metadata do not add another native call");
        assert_eq!(retained.context, original);
        assert_eq!(retained.statement.text(), Some("SELECT id FROM @Rows WHERE id > 10"));
        assert_eq!(retained.module_parameters.is_some(), !changed);
        if changed {
            assert!(matches!(retained.payload, Payload::Unavailable(PayloadUnavailable::SourceUnavailable)));
        } else {
            let plan = super::super::plan_from_retained(
                Read { setting_on: Some(true), plan: Some(retained) },
                &crate::plan_xml::test_read_context("app"),
            )
            .unwrap();
            assert_eq!(
                plan.root.facts().relational_input.unwrap().qualified_kind(),
                culprit_core::plan_facts::RelationalInputKind::TableValuedParameter
            );
            assert_eq!(plan.root.facts().access_rows.unwrap().actual_total, Some(10000.0));
        }
    }
}

#[tokio::test]
async fn unknown_native_tokens_preserve_raw_counters_without_an_attribute_lookup() {
    for name in ["plan_handle", "sql_handle"] {
        let native = replace(&row(), &[(name, Val::Text("0x0000".into()))]);
        let mut reader =
            FakeRead::new(vec![Ok(vec![layout()]), Ok(vec![Rec::of(&[("value", Val::Int(1))])]), Ok(vec![native])]);
        let observed = read(&mut reader, &request(None), 1024).await.unwrap().plan.unwrap();
        assert_eq!(observed.executions, "2");
        assert!(matches!(observed.payload, Payload::Unavailable(PayloadUnavailable::NotCollected)));
        assert_eq!(reader.calls.len(), 3);
    }
}

#[tokio::test]
async fn terminal_final_proof_errors_do_not_become_missing_statistics_or_trigger_followup_reads() {
    for stop in [
        culprit_core::telemetry::CollectionStop::TimedOut,
        culprit_core::telemetry::CollectionStop::CancellationUnconfirmed,
        culprit_core::telemetry::CollectionStop::ByteLimit,
    ] {
        let mut reader = FakeRead::new(vec![
            Ok(vec![layout()]),
            Ok(vec![Rec::of(&[("value", Val::Int(1))])]),
            Ok(vec![row()]),
            Err(ReadError::Stopped(stop)),
        ]);
        let error = read(&mut reader, &request(None), 1024).await.err().unwrap();
        assert_eq!(culprit_core::telemetry::CollectionStop::from_error(&error), stop);
        assert_eq!(reader.calls.len(), 4);
    }
}

#[tokio::test]
async fn full_query_and_plan_identities_survive_the_existing_single_payload_lookup() {
    let native = row();
    let query = crate::query_store::decode::prefixed_query_identity(&native, "query_store_").unwrap();
    let plan =
        crate::query_store::decode::plan_identity_for_query(&native, "query_store_", query.clone()).unwrap().unwrap();
    let mut selected = request(Some("9"));
    selected.expected_query = Some(query.clone());
    selected.expected_plan = Some(plan.clone());
    let mut reader = payload_reader(native);
    let result = read(&mut reader, &selected, 1024).await.unwrap().plan.unwrap();
    assert_eq!(result.query, Some(query));
    assert_eq!(result.native_plan, Some(plan));
    assert_eq!(reader.queries(), 4);
    assert!(reader.calls[2].sql.contains("qt.statement_sql_handle=qs.statement_sql_handle"));
    assert!(reader.calls[2].sql.contains("q.context_settings_id=qs.statement_context_id"));
    assert!(!reader.calls[2].sql.contains("sys.fn_stmt_sql_handle_from_sql_stmt"));
}

#[tokio::test]
async fn a_reused_query_number_never_bypasses_full_statement_incarnation_matching() {
    let native = row();
    let original = crate::query_store::decode::prefixed_query_identity(&native, "query_store_").unwrap();
    for (name, value) in [
        ("context_settings_id", "6"),
        ("object_id", "100"),
        ("query_hash", "FEDCBA9876543210"),
        ("initial_compile_at", "2019-01-01T06:00:00.000000000Z"),
    ] {
        let mut selected = request(None);
        let mut expected = original.clone();
        expected.qualifiers.insert(name.into(), value.into());
        selected.expected_query = Some(expected);
        let mut reader = payload_reader(native.clone());
        let error = read(&mut reader, &selected, 1024).await.err().unwrap();
        assert_eq!(
            culprit_core::telemetry::CollectionStop::from_error(&error),
            culprit_core::telemetry::CollectionStop::SourceChanged,
            "{name}"
        );
        assert_eq!(reader.queries(), 3, "a contradicted statement cannot request a later plan binding");
    }
}

#[tokio::test]
async fn a_reused_plan_number_or_missing_native_identity_does_not_fall_back_to_shape_hashes() {
    let native = row();
    let query = crate::query_store::decode::prefixed_query_identity(&native, "query_store_").unwrap();
    let original =
        crate::query_store::decode::plan_identity_for_query(&native, "query_store_", query).unwrap().unwrap();
    for (name, value) in [("initial_compile_at", "2019-01-01T06:00:00.000000000Z"), ("plan_hash", "FFEEDDCCBBAA9988")] {
        let mut selected = request(Some("9"));
        let mut expected = original.clone();
        expected.qualifiers.insert(name.into(), value.into());
        selected.expected_plan = Some(expected);
        let mut reader = payload_reader(native.clone());
        let error = read(&mut reader, &selected, 1024).await.err().unwrap();
        assert_eq!(
            culprit_core::telemetry::CollectionStop::from_error(&error),
            culprit_core::telemetry::CollectionStop::SourceChanged,
            "{name}"
        );
        assert_eq!(reader.queries(), 3);
    }
    let mut reader = payload_reader(replace(&native, &[("query_store_context_settings_id", Val::Null)]));
    let error = read(&mut reader, &request(None), 1024).await.err().unwrap();
    assert_eq!(
        culprit_core::telemetry::CollectionStop::from_error(&error),
        culprit_core::telemetry::CollectionStop::InvalidPage
    );
    assert_eq!(reader.queries(), 3);
}

#[tokio::test]
async fn invalid_or_contradictory_selectors_stop_before_any_native_metadata_work() {
    for selected in [
        RetainedActualPlanRequest { query_id: "not-a-number".into(), ..request(None) },
        RetainedActualPlanRequest { query_id: "99999999999999999999999999999".into(), ..request(None) },
        RetainedActualPlanRequest {
            expected_query: Some(NativeQueryIdentity { query_id: "8".into(), qualifiers: Default::default() }),
            ..request(None)
        },
    ] {
        let mut reader = FakeRead::new(vec![]);
        assert!(read(&mut reader, &selected, 1024).await.is_err());
        assert_eq!(reader.queries(), 0);
    }
}

fn payload_reader(native: Rec) -> FakeRead {
    FakeRead::new(vec![
        Ok(vec![layout()]),
        Ok(vec![Rec::of(&[("value", Val::Int(1))])]),
        Ok(vec![native.clone()]),
        Ok(vec![crate::relational_inputs::tests::binding_row(&cached_statements::context(&native).unwrap())]),
    ])
}

fn owned_payload() -> Rec {
    let retained = super::super::tests::retained_row();
    let xml = retained.payload.text().unwrap();
    replace(
        &row(),
        &[
            ("database_id", Val::Int(7)),
            ("query_plan", Val::Text(xml.to_owned())),
            ("plan_bytes", Val::Int(xml.encode_utf16().count() as i64 * 2)),
        ],
    )
}

fn owned_reader(native: Rec, after: crate::read::ReadResult<Vec<Rec>>) -> FakeRead {
    let platform = replace(
        &crate::instance::test_support::metadata(),
        &[("edition", Val::Int(5)), ("machine_name", Val::Null), ("process_id", Val::Null)],
    );
    FakeRead::new(vec![
        Ok(vec![platform]),
        Ok(vec![crate::activity::database_context::tests::row(5)]),
        Ok(vec![layout()]),
        Ok(vec![Rec::of(&[("value", Val::Int(1))])]),
        Ok(vec![native.clone()]),
        Ok(vec![crate::relational_inputs::tests::binding_row(&cached_statements::context(&native).unwrap())]),
        after,
    ])
}

#[tokio::test]
async fn retained_plan_provenance_keeps_read_owner_and_statement_identity_without_borrowing_execution_time() {
    let native = owned_payload();
    let mut reader = owned_reader(native.clone(), Ok(vec![crate::activity::database_context::tests::row(5)]));
    let result =
        super::super::read_retained_plan(&mut reader, "fixture.invalid", 1433, &request(Some("9")), 2 * 1024 * 1024)
            .await
            .unwrap();
    assert_eq!(result.provenance.statement, cached_statements::context(&native).unwrap());
    assert_eq!(result.provenance.query.as_ref().unwrap().query_id, "7");
    assert_eq!(result.provenance.native_plan.as_ref().unwrap().plan_id, "9");
    assert!(result.provenance.read.partition.is_qualified());
    assert_eq!(result.plan.kind, culprit_core::PlanKind::Actual);
    assert!(result.plan.facts().last_run_at.is_none());
    assert!(!result.plan.props.contains_key("CachedExecutions"));
    assert_eq!(reader.queries(), 7);
    assert_eq!(reader.closed, 0);
}

#[tokio::test]
async fn an_expected_database_mismatch_stops_after_native_metadata_without_reading_plan_layout_or_payload() {
    let mut expected =
        crate::activity::database_context::test_azure_authority(&crate::activity::database_context::tests::row(5))
            .unwrap()
            .activity
            .database;
    expected.incarnation = Some("another-logical-database-incarnation".into());
    let mut selected = request(Some("9"));
    selected.expected_target = Some(expected.into());
    let native = row();
    let query = crate::query_store::decode::prefixed_query_identity(&native, "query_store_").unwrap();
    selected.expected_plan =
        crate::query_store::decode::plan_identity_for_query(&native, "query_store_", query.clone()).unwrap();
    selected.expected_query = Some(query);
    let mut reader = owned_reader(owned_payload(), Ok(vec![crate::activity::database_context::tests::row(5)]));
    let error = super::super::read_retained_plan(&mut reader, "fixture.invalid", 1433, &selected, 2 * 1024 * 1024)
        .await
        .err()
        .unwrap();
    assert_eq!(
        culprit_core::telemetry::CollectionStop::from_error(&error),
        culprit_core::telemetry::CollectionStop::SourceChanged
    );
    assert_eq!(reader.queries(), 2);
    assert!(reader.calls.iter().all(|call| !call.sql.contains("sys.dm_exec_query_plan_stats")
        && !call.sql.contains("sys.query_store_query")
        && !call.sql.contains("LAST_QUERY_PLAN_STATS")));
    assert_eq!(reader.closed, 0);
}

#[tokio::test]
async fn a_changed_connected_member_or_role_withholds_a_previously_read_actual_plan() {
    for (name, value) in [
        ("replica_id", Val::Text("11111111-2222-3333-4444-555555555555".into())),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("local_process_id", Val::Int(12)),
    ] {
        let after = replace(&crate::activity::database_context::tests::row(5), &[(name, value)]);
        let mut reader = owned_reader(owned_payload(), Ok(vec![after]));
        let error = super::super::read_retained_plan(
            &mut reader,
            "fixture.invalid",
            1433,
            &request(Some("9")),
            2 * 1024 * 1024,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            culprit_core::telemetry::CollectionStop::from_error(&error),
            culprit_core::telemetry::CollectionStop::SourceChanged,
            "{name}"
        );
        assert_eq!(reader.queries(), 7);
    }
}

#[tokio::test]
async fn final_owner_shutdown_uncertainty_is_preserved_after_the_retained_payload_and_binding_succeeded() {
    let mut reader = owned_reader(
        owned_payload(),
        Err(ReadError::Stopped(culprit_core::telemetry::CollectionStop::CancellationUnconfirmed)),
    );
    let error =
        super::super::read_retained_plan(&mut reader, "fixture.invalid", 1433, &request(Some("9")), 2 * 1024 * 1024)
            .await
            .err()
            .unwrap();
    assert_eq!(
        culprit_core::telemetry::CollectionStop::from_error(&error),
        culprit_core::telemetry::CollectionStop::CancellationUnconfirmed
    );
    assert_eq!(reader.queries(), 7);
    assert_eq!(reader.closed, 0, "the caller retains responsibility for terminal native settlement");
}
