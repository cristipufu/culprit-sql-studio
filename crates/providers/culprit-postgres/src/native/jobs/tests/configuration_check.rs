use super::*;

fn changed_catalog(change: impl FnOnce(&mut Vec<serde_json::Value>)) -> Vec<Row> {
    let mut catalog = catalog();
    let mut columns = serde_json::from_str(&catalog[0].text("columns").unwrap()).unwrap();
    change(&mut columns);
    catalog[0] = replace(&catalog[0], "columns", Some(&serde_json::to_string(&columns).unwrap()));
    catalog
}

fn state_point(captured: &TimescaleJobsSnapshot) -> &TimescaleJobState {
    captured
        .observations
        .iter()
        .find_map(|row| match &row.fact {
            OperationalFact::TimescaleJobState(point) => Some(point.as_ref()),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn configuration_validation_labels_use_the_same_bounded_select_without_reading_or_executing_config() {
    let mut io = reader();
    let snapshot = capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap();
    assert_eq!(
        state_point(&snapshot).configuration_check,
        Some(TimescaleJobProcedure { schema: "public".into(), name: "validate_owned_configuration".into() })
    );
    assert_eq!(io.calls.len(), 6);
    let sql = &io.calls[2];
    assert!(sql.contains("j.check_schema::text AS check_schema"));
    assert!(sql.contains("j.check_name::text AS check_name"));
    assert!(sql.ends_with("ORDER BY j.job_id LIMIT 257"));
    for forbidden in [
        "j.config",
        "application_name",
        "err_message",
        "validate_owned_configuration",
        "pg_stat_activity",
        "hypertable_detailed_size",
        "chunks_detailed_size",
        "pg_relation_size",
    ] {
        assert!(!sql.contains(forbidden), "{forbidden}");
    }
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Collected);
    assert_eq!(snapshot.metadata.successful_runs_logged, Some(false));
    assert_eq!(snapshot.context.partition, SourcePartition::Target);
}

#[tokio::test]
async fn older_supported_views_and_native_null_keep_the_function_unknown_without_a_fallback_query() {
    for supported in [true, false] {
        let mut io = reader();
        if !supported {
            let catalog = changed_catalog(|columns| {
                columns.retain(|column| !matches!(column["name"].as_str(), Some("check_schema" | "check_name")))
            });
            io.replies[1] = Ok(catalog.clone());
            io.replies[5] = Ok(catalog);
        }
        io.replies[2] = Ok(vec![state_with_configuration_check(None, None)]);
        let snapshot =
            capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap();
        assert!(state_point(&snapshot).configuration_check.is_none());
        assert_eq!(io.calls.len(), 6);
        assert_eq!(snapshot.metadata.view(TimescaleJobViewKind::Jobs).unwrap().state, TimescaleJobViewState::Available);
        assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Collected);
        if !supported {
            assert!(io.calls[2].contains("NULL::text AS check_schema"));
            assert!(io.calls[2].contains("NULL::text AS check_name"));
            assert!(!io.calls[2].contains("j.check_schema") && !io.calls[2].contains("j.check_name"));
        }
    }
}

#[tokio::test]
async fn denied_wrong_type_or_half_column_capability_never_starts_the_job_state_select() {
    for shape in ["denied", "wrong-type", "half-column"] {
        let mut io = reader();
        let mut catalog = changed_catalog(|columns| match shape {
            "wrong-type" => {
                columns.iter_mut().find(|column| column["name"] == "check_name").unwrap()["oid"] =
                    serde_json::json!(114)
            }
            "half-column" => columns.retain(|column| column["name"] != "check_schema"),
            _ => {}
        });
        if shape == "denied" {
            catalog[0] = replace(&catalog[0], "can_select", Some("f"));
        }
        io.replies[1] = Ok(catalog.clone());
        io.replies[5] = Ok(catalog);
        io.replies.remove(2);
        let snapshot =
            capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap();
        assert_eq!(io.calls.len(), 5);
        assert!(io.calls.iter().all(|sql| !sql.contains("FROM timescaledb_information.jobs j")));
        assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Unavailable);
        assert_eq!(
            snapshot.sources[0].error_code,
            Some(if shape == "denied" { ErrorCode::PermissionDenied } else { ErrorCode::Unsupported })
        );
        assert!(snapshot.observations.iter().all(|row| !matches!(row.fact, OperationalFact::TimescaleJobState(_))));
    }
}

#[tokio::test]
async fn incomplete_or_oversized_native_labels_fail_before_history_or_final_followups() {
    let too_long = "x".repeat(MAX_TIMESCALE_JOB_LABEL_BYTES + 1);
    for (schema, name) in [
        (Some("public"), None),
        (None, Some("validate")),
        (Some(""), Some("validate")),
        (Some("public"), Some(too_long.as_str())),
    ] {
        let mut io = reader();
        io.replies[2] = Ok(vec![state_with_configuration_check(schema, name)]);
        assert!(capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.is_err());
        assert_eq!(io.calls.len(), 3);
    }
}

#[tokio::test]
async fn changed_original_check_column_identity_cannot_seal_provisional_job_metadata() {
    let mut io = reader();
    io.replies[5] = Ok(changed_catalog(|columns| {
        columns.iter_mut().find(|column| column["name"] == "check_name").unwrap()["oid"] = serde_json::json!(25)
    }));
    let error =
        capture(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert_eq!(io.calls.len(), 6);
}

#[tokio::test]
async fn a_partial_job_population_keeps_the_recorded_check_without_enlarging_the_query_or_run_budget() {
    let mut io = reader();
    io.replies[2] = Ok(vec![state(), state()]);
    io.replies.remove(3);
    let budget = CollectionBudget { max_snapshot_rows: 1, ..bounded_budget(&CollectionBudget::default()) };
    let snapshot = capture(&mut io, "fixture.invalid", 9, &budget).await.unwrap();
    assert!(state_point(&snapshot).configuration_check.is_some());
    assert_eq!(snapshot.sources[0].stopped, Some(CollectionStop::RowLimit));
    assert_eq!(snapshot.sources[1].stopped, Some(CollectionStop::RowLimit));
    assert!(io.calls[2].ends_with("LIMIT 2"));
    assert_eq!(io.calls.len(), 5);
}
