use super::*;
use test_support::*;
#[tokio::test]
async fn installed_columns_and_exact_native_facts_survive_one_bounded_owner_without_worker_attribution() {
    let mut io = reader();
    let captured = capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap();
    assert_eq!(io.calls.len(), 6);
    assert_eq!(captured.metadata.run_visibility, TimescaleJobRunVisibility::JobOwners);
    assert_eq!(captured.metadata.successful_runs_logged, Some(false));
    assert!(!captured.context.has_qualified_local_owner());
    assert!(!captured.sources[1].complete_scope);
    let OperationalFact::TimescaleJobState(state) = &captured.observations[0].fact else { panic!() };
    assert_eq!(state.statistics.as_ref().unwrap().total_runs.as_deref(), Some("9007199254740993"));
    assert_eq!(state.initial_start, Some(TimescaleJobTimestamp::NegativeInfinity));
    assert_eq!(state.next_start, Some(TimescaleJobTimestamp::PositiveInfinity));
    let OperationalFact::TimescaleJobRun(run) = &captured.observations[1].fact else { panic!() };
    assert_eq!(run.failure_kind(), None);
    assert_eq!(run.procedure.as_ref().unwrap().origin, TimescaleJobProcedureOrigin::StoredOrCurrent);
    for sql in &io.calls[2..4] {
        assert!(!sql.contains("err_message"));
        assert!(!sql.contains("j.config"));
        assert!(!sql.contains("application_name"));
        assert!(!sql.contains("pg_stat_activity"));
    }
}
#[tokio::test]
async fn caller_row_limit_retains_original_positions_and_marks_both_truncation_and_skipped_runs() {
    let mut io = reader();
    io.replies[2] = Ok(vec![state(), state()]);
    io.replies.remove(3);
    let budget = CollectionBudget { max_snapshot_rows: 1, ..bounded_budget(&CollectionBudget::default()) };
    let captured = capture(&mut io, "fixture.invalid", 9, &budget).await.unwrap();
    assert_eq!(captured.observations.len(), 1);
    assert_eq!(captured.sources[0].stopped, Some(CollectionStop::RowLimit));
    assert_eq!(captured.sources[1].stopped, Some(CollectionStop::RowLimit));
    assert_eq!(captured.sources[1].state, culprit_core::SourceState::Unavailable);
    assert_eq!(io.calls.len(), 5);
}
#[tokio::test]
async fn errors_stop_all_followup_and_final_catalog_or_native_generation_changes_discard_provisional_points() {
    for failure in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::QueryLimit] {
        let mut io = reader();
        io.replies[2] = Err(failure.error());
        let error =
            capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), failure);
        assert_eq!(io.calls.len(), 3);
    }
    let mut io = reader();
    let mut changed = catalog();
    changed[0] = replace(&changed[0], "view_oid", Some("29000"));
    io.replies[5] = Ok(changed);
    assert_eq!(
        CollectionStop::from_error(
            &capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap_err()
        ),
        CollectionStop::SourceChanged
    );
    let mut io = reader();
    io.replies[4] = Ok(vec![crate::progress::test_support::metadata_with("f", "2026-11-15T10:00:00Z")]);
    assert_eq!(
        CollectionStop::from_error(
            &capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap_err()
        ),
        CollectionStop::SourceChanged
    );
    assert_eq!(io.calls.len(), 5, "failed final native authority cannot start another source query");
}
#[tokio::test]
async fn permission_capability_skips_the_denied_view_and_selects_errors_without_fabricating_succeeded() {
    let mut io = reader();
    let mut catalog = catalog();
    catalog[2] = replace(&catalog[2], "can_select", Some("f"));
    io.replies[1] = Ok(catalog.clone());
    io.replies[5] = Ok(catalog);
    let row = crate::progress::test_support::row(&[
        ("observed_at", Some(AT)),
        ("native_run_id", None),
        ("job_id", Some("1000")),
        ("process_id", None),
        ("proc_schema", Some("public")),
        ("proc_name", Some("reported_label")),
        ("started_at", Some("2026-11-15T09:00:00Z")),
        ("finished_at", None),
        ("succeeded", None),
        ("sql_state", None),
    ]);
    io.replies[3] = Ok(vec![row]);
    let captured = capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap();
    assert_eq!(captured.metadata.run_view, Some(TimescaleJobViewKind::Errors));
    assert!(io.calls[3].contains("timescaledb_information.job_errors r"));
    assert!(!io.calls[3].contains("pg_stat_activity"));
    assert!(captured.sources.iter().all(|source| source.error_code != Some(ErrorCode::QueryFailed)));
}

#[tokio::test]
async fn absent_extension_and_denied_views_never_construct_a_native_data_select() {
    let mut io = reader();
    let absent = catalog()
        .iter()
        .map(|row| {
            let row = replace(row, "extension_oid", None);
            let row = replace(&row, "extension_version", None);
            let row = replace(&row, "view_oid", None);
            let row = replace(&row, "columns", None);
            replace(&row, "can_select", None)
        })
        .collect::<Vec<_>>();
    io.replies[1] = Ok(absent.clone());
    io.replies[5] = Ok(absent);
    io.replies.remove(3);
    io.replies.remove(2);
    let snapshot = capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap();
    assert_eq!(io.calls.len(), 4);
    assert!(snapshot.observations.is_empty());
    assert!(snapshot.sources.iter().all(|source| source.state == culprit_core::SourceState::NotApplicable));
    assert!(io.calls.iter().all(|sql| !sql.contains("FROM timescaledb_information.")));
    let mut io = reader();
    let denied = catalog().iter().map(|row| replace(row, "can_select", Some("f"))).collect::<Vec<_>>();
    io.replies[1] = Ok(denied.clone());
    io.replies[5] = Ok(denied);
    io.replies.remove(3);
    io.replies.remove(2);
    let snapshot = capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap();
    assert_eq!(io.calls.len(), 4);
    assert!(snapshot.sources.iter().all(|source| source.state == culprit_core::SourceState::Unavailable));
}
#[tokio::test]
async fn optional_columns_are_projected_as_unknown_and_malformed_required_native_types_never_enter_queries() {
    let view = discovery::View {
        capability: TimescaleJobViewCapability {
            kind: TimescaleJobViewKind::Errors,
            state: TimescaleJobViewState::Available,
            identity: None,
        },
        columns: BTreeMap::from([("job_id".into(), 23), ("start_time".into(), 1184), ("finish_time".into(), 1184)]),
    };
    let sql = sql::runs(
        &view,
        &TimescaleJobRunWindow { from: "2026-11-14T00:00:00Z".into(), until: "2026-11-15T00:00:00Z".into() },
        4,
    );
    assert!(!sql.contains("r.pid"));
    assert!(!sql.contains("r.proc_schema"));
    assert!(!sql.contains("r.sqlerrcode"));
    assert!(sql.contains("NULL::text AS succeeded"));
    let mut fields = catalog();
    fields[0] = replace(&fields[0], "columns", Some(r#"[{"name":"job_id","oid":25}]"#));
    let mut io = reader();
    io.replies[1] = Ok(fields.clone());
    io.replies[5] = Ok(fields);
    io.replies.remove(2);
    let snapshot = capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap();
    assert_eq!(
        snapshot.metadata.view(TimescaleJobViewKind::Jobs).unwrap().state,
        TimescaleJobViewState::UnsupportedColumns
    );
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Unavailable);
    assert_eq!(snapshot.observations.len(), 1);
    assert!(!io.calls.iter().any(|sql| sql.contains("FROM timescaledb_information.jobs j")));
}

#[tokio::test]
async fn only_the_verified_interface_strengthens_reported_procedure_label_provenance() {
    let mut io = reader();
    let mut known = catalog();
    known[2] = replace(&known[2], "can_select", Some("f"));
    io.replies[1] = Ok(known.clone());
    io.replies[5] = Ok(known);
    let original = run();
    let fields =
        ["observed_at", "job_id", "process_id", "proc_schema", "proc_name", "started_at", "finished_at", "sql_state"];
    let row = Row::from_fields(
        fields
            .into_iter()
            .map(|key| (key.into(), original.text(key)))
            .chain([("native_run_id".into(), None), ("succeeded".into(), None)]),
    );
    io.replies[3] = Ok(vec![row]);
    let captured = capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap();
    let OperationalFact::TimescaleJobRun(point) = &captured.observations[1].fact else { panic!() };
    assert_eq!(point.procedure.as_ref().unwrap().origin, TimescaleJobProcedureOrigin::Stored);
    let mut io = reader();
    let unknown = catalog().iter().map(|row| replace(row, "extension_version", Some("2.99.0"))).collect::<Vec<_>>();
    io.replies[1] = Ok(unknown.clone());
    io.replies[5] = Ok(unknown);
    let captured = capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap();
    let OperationalFact::TimescaleJobRun(point) = &captured.observations[1].fact else { panic!() };
    assert_eq!(point.procedure.as_ref().unwrap().origin, TimescaleJobProcedureOrigin::Unknown);
}
