use super::{test_support::*, *};

async fn capture(io: &mut Reader) -> Result<MaintenanceProgressSnapshot> {
    capture_from(io, "fixture.invalid", 9, "42", Some(&original_focus()), &bounded_budget(&CollectionBudget::default()))
        .await
}

#[tokio::test]
async fn manual_vacuum_retains_legacy_and_new_native_counters_without_manufacturing_missing_values() {
    for version in [110_000, 160_000, 170_000, 180_000] {
        let empty = usize::from(version >= 120_000);
        let mut io = progress_reader(version, empty, Some(vacuum_progress(version, &[])));
        let captured = capture(&mut io).await.unwrap();
        assert!(io.replies.is_empty());
        assert_eq!(io.calls.len(), 7 + empty);
        assert_eq!(captured.source, vacuum::SOURCE);
        assert_eq!(captured.coverage.source, vacuum::SOURCE);
        assert!(!captured.coverage.complete_scope);
        let point = &captured.observations[0];
        assert_eq!(point.actor, original_focus().original.key);
        assert_eq!(point.statement, Some(original_focus().statement_identity));
        assert_eq!(point.command, MaintenanceCommand::Vacuum);
        assert_eq!(point.index_oid, None);
        assert_eq!(point.relation_reference().unwrap().id, "16420");
        let MaintenanceProgressWork::PostgreSqlVacuum { progress } = &point.work else { panic!() };
        assert_eq!(progress.heap_blks_total.as_deref(), Some("9007199254740993"));
        assert_eq!(progress.heap_blks_vacuumed.as_deref(), Some("0"));
        assert_eq!(progress.index_vacuum_count.as_deref(), Some("0"));
        if version < 170_000 {
            assert_eq!(progress.max_dead_tuples.as_deref(), Some("291"));
            assert_eq!(progress.num_dead_tuples.as_deref(), Some("0"));
            assert_eq!(progress.dead_tuple_bytes, None);
            assert_eq!(progress.num_dead_item_ids, None);
            assert_eq!(progress.indexes_total, None);
        } else {
            assert_eq!(progress.max_dead_tuples, None);
            assert_eq!(progress.num_dead_tuples, None);
            assert_eq!(progress.max_dead_tuple_bytes.as_deref(), Some("9007199254740993"));
            assert_eq!(progress.dead_tuple_bytes.as_deref(), Some("0"));
            assert_eq!(progress.num_dead_item_ids.as_deref(), Some("1"));
            assert_eq!(progress.indexes_total.as_deref(), Some("2"));
            assert_eq!(progress.indexes_processed.as_deref(), Some("0"));
        }
        assert_eq!(progress.cost_delay_ms.as_deref(), (version >= 180_000).then_some("0.125"));
        let sql = io.calls.iter().find(|sql| sql.contains("FROM pg_catalog.pg_stat_progress_vacuum")).unwrap();
        assert_eq!(sql.contains("p.max_dead_tuples::text"), version < 170_000);
        assert_eq!(sql.contains("p.dead_tuple_bytes::text"), version >= 170_000);
        assert_eq!(sql.contains("p.delay_time::text"), version >= 180_000);
        assert!(!sql.contains("current_setting"), "the collector's delay setting cannot qualify another backend");
    }
}

#[tokio::test]
async fn analyze_retains_sampling_statistics_and_child_oid_as_independent_native_facts() {
    let mut io = progress_reader(180_000, 2, Some(analyze_progress(&[])));
    let captured = capture(&mut io).await.unwrap();
    assert_eq!(captured.source, analyze::SOURCE);
    assert_eq!(io.calls.len(), 9);
    let point = &captured.observations[0];
    assert_eq!(point.command, MaintenanceCommand::Analyze);
    assert_eq!(point.index_oid, None);
    let MaintenanceProgressWork::PostgreSqlAnalyze { progress } = &point.work else { panic!() };
    assert_eq!(progress.sample_blks_total.as_deref(), Some("9007199254740993"));
    assert_eq!(progress.sample_blks_scanned.as_deref(), Some("0"));
    assert_eq!(progress.ext_stats_total.as_deref(), Some("2"));
    assert_eq!(progress.ext_stats_computed, None);
    assert_eq!(progress.child_tables_total.as_deref(), Some("4"));
    assert_eq!(progress.child_tables_done.as_deref(), Some("1"));
    assert_eq!(progress.current_child_table_relid.as_deref(), Some("16421"));
    assert_eq!(progress.cost_delay_ms.as_deref(), Some("0"));
    assert_eq!(phase_fraction(point), None, "inherited sampling is not whole-command progress");
    assert!(!io.calls.iter().any(|sql| sql.contains("pg_stat_progress_cluster")));
}

#[tokio::test]
async fn cluster_and_vacuum_full_use_native_command_and_preserve_optional_index_reference() {
    for (command, expected, index) in
        [("CLUSTER", MaintenanceCommand::Cluster, "16422"), ("VACUUM FULL", MaintenanceCommand::VacuumFull, "0")]
    {
        let mut io = progress_reader(180_000, 3, Some(cluster_progress(command, &[("index_oid", Some(index))])));
        let captured = capture(&mut io).await.unwrap();
        assert_eq!(io.calls.len(), 10);
        assert_eq!(captured.source, cluster::SOURCE);
        let point = &captured.observations[0];
        assert_eq!(point.command, expected);
        assert_eq!(point.index_oid.as_deref(), Some(index));
        assert_eq!(point.index_reference().map(|identity| identity.id), (index != "0").then(|| index.into()));
        let MaintenanceProgressWork::PostgreSqlCluster { progress } = &point.work else { panic!() };
        assert_eq!(progress.heap_tuples_scanned.as_deref(), Some("9007199254740993"));
        assert_eq!(progress.heap_tuples_written.as_deref(), Some("0"));
        assert_eq!(progress.heap_blks_total, None);
        assert_eq!(progress.index_rebuild_count.as_deref(), Some("1"));
        assert_eq!(phase_fraction(point), None, "index rebuilding does not inherit heap-scan completion");
    }
}

#[tokio::test]
async fn all_empty_views_are_nonexhaustive_and_optional_view_denial_is_not_completion() {
    for denied in [false, true] {
        let mut io = progress_reader(180_000, 4, None);
        if denied {
            io.replies[3] = Err(Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denied"));
        }
        let captured = capture(&mut io).await.unwrap();
        assert_eq!(io.calls.len(), 10, "the admitted profile contains at most four selected view reads");
        assert!(captured.observations.is_empty());
        assert!(!captured.coverage.complete_scope);
        assert_eq!(captured.coverage.records, 0);
        assert_eq!(captured.source, maintenance::SOURCE);
        assert_eq!(
            captured.result.membership,
            if denied { FocusedRequestMembership::Unavailable } else { FocusedRequestMembership::NotObserved }
        );
        assert_eq!(
            captured.coverage.state,
            if denied { culprit_core::SourceState::Unavailable } else { culprit_core::SourceState::Partial }
        );
        assert!(!io.calls.iter().any(|sql| sql.contains("pg_stat_progress_copy")));
    }
}

#[tokio::test]
async fn one_optional_view_denial_does_not_hide_a_later_native_maintenance_point() {
    let mut io = progress_reader(180_000, 1, Some(vacuum_progress(180_000, &[])));
    io.replies[3] = Err(Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denied"));
    let captured = capture(&mut io).await.unwrap();
    assert_eq!(captured.source, vacuum::SOURCE);
    assert_eq!(captured.result.membership, FocusedRequestMembership::Observed);
    assert_eq!(captured.coverage.state, culprit_core::SourceState::Partial);
}

#[tokio::test]
async fn non_index_points_keep_original_request_member_process_and_role_guards() {
    for (empty, point) in
        [(1, vacuum_progress(180_000, &[])), (2, analyze_progress(&[])), (3, cluster_progress("CLUSTER", &[]))]
    {
        for changed in ["request", "backend", "member", "process", "role"] {
            let mut io = progress_reader(180_000, empty, Some(point.clone()));
            match changed {
                "request" => io.replies[4 + empty] = Ok(vec![actor(BACKEND, "2026-11-15T09:00:01Z")]),
                "backend" => io.replies[4 + empty] = Ok(vec![actor("2026-11-15T08:00:01Z", REQUEST)]),
                "member" => io.replies[6 + empty] = Ok(vec![member("127.0.0.2")]),
                "process" => io.replies[5 + empty] = Ok(vec![metadata_with("f", "2026-11-14T00:00:01Z")]),
                _ => io.replies[5 + empty] = Ok(vec![metadata_with("t", "2026-11-14T00:00:00Z")]),
            }
            assert_eq!(
                CollectionStop::from_error(&capture(&mut io).await.unwrap_err()),
                CollectionStop::SourceChanged,
                "{changed} after {empty} empty views"
            );
        }
    }
}

#[tokio::test]
async fn typed_stops_and_stricter_query_limits_never_start_a_fallback_or_final_read() {
    for stop in
        [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::QueryLimit, CollectionStop::ByteLimit]
    {
        let mut io = progress_reader(180_000, 1, Some(vacuum_progress(180_000, &[])));
        io.replies[4] = Err(stop.error());
        assert_eq!(CollectionStop::from_error(&capture(&mut io).await.unwrap_err()), stop);
        assert_eq!(io.calls.len(), 5);
        assert_eq!(io.replies.len(), 3, "no actor/owner recheck or another view follows a terminal stop");
    }
    let mut io = progress_reader(180_000, 4, None);
    io.query_limit = Some(4);
    assert_eq!(CollectionStop::from_error(&capture(&mut io).await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(io.calls.len(), 4);
    assert!(io.calls[3].contains("pg_stat_progress_create_index"));
}

#[tokio::test]
async fn malformed_or_cross_actor_non_index_rows_fail_before_any_fallback() {
    for changes in [
        vec![("heap_blks_scanned", Some("-1"))],
        vec![("dead_tuple_bytes", Some("01"))],
        vec![("pid", Some("43"))],
        vec![("query_start", Some("2026-11-15T09:00:01Z"))],
        vec![("database_oid", Some("16385"))],
        vec![("cost_delay_ms", Some("NaN"))],
        vec![("cost_delay_ms", Some("-0.5"))],
    ] {
        let mut io = progress_reader(180_000, 1, Some(vacuum_progress(180_000, &changes)));
        assert!(capture(&mut io).await.is_err(), "{changes:?}");
        assert_eq!(io.calls.len(), 5);
    }
    let mut io = progress_reader(180_000, 3, Some(cluster_progress("CREATE INDEX", &[])));
    assert_eq!(CollectionStop::from_error(&capture(&mut io).await.unwrap_err()), CollectionStop::InvalidPage);
    assert_eq!(io.calls.len(), 7);
    let mut io = progress_reader(180_000, 1, Some(native_progress(&[("phase", Some("scanning heap"))])));
    assert_eq!(CollectionStop::from_error(&capture(&mut io).await.unwrap_err()), CollectionStop::InvalidPage);
    assert_eq!(io.calls.len(), 5, "absent native aliases are malformed, not unknown or zero counters");
}

#[tokio::test]
async fn manual_scope_uses_native_backend_type_and_never_infers_command_from_current_sql() {
    let mut io = progress_reader(180_000, 1, Some(vacuum_progress(180_000, &[])));
    let captured = capture(&mut io).await.unwrap();
    assert_eq!(captured.observations[0].command, MaintenanceCommand::Vacuum);
    for sql in io.calls.iter().filter(|sql| sql.contains("FROM stamp LEFT JOIN LATERAL")) {
        assert!(sql.contains("backend_type IN ('client backend')"));
        assert!(!sql.contains("current_sql"), "retained progress does not acquire SQL bodies");
    }
    let progress_sql = &io.calls[4];
    for guard in [
        "p.pid=42",
        "p.datid=16384",
        "a.backend_start=",
        "a.query_start=",
        "a.state='active'",
        "a.backend_type='client backend'",
        "LIMIT 2",
    ] {
        assert!(progress_sql.contains(guard), "{guard}");
    }
    let mut old = Reader {
        replies: [Ok(vec![metadata_on_version(90_600, "f", "2026-11-14T00:00:00Z")])].into(),
        ..Default::default()
    };
    assert_eq!(capture(&mut old).await.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
    assert_eq!(old.calls.len(), 1, "no actor or progress query without native client-backend classification");
}

#[test]
fn version_projection_never_references_columns_absent_from_that_native_view_version() {
    let actor = original_focus().original.key;
    let sql = analyze::sql("16384", &actor, 130_000).unwrap();
    assert!(sql.contains("p.current_child_table_relid::text"));
    assert!(sql.contains("NULL::text AS cost_delay_ms"));
    assert!(!sql.contains("p.delay_time"));
    assert!(analyze::sql("16384", &actor, 180_000).unwrap().contains("p.delay_time::text AS cost_delay_ms"));
}

#[tokio::test]
async fn live_copy_retains_native_current_work_without_a_fake_phase_or_completion_percentage() {
    let mut io = progress_reader(
        180_000,
        4,
        Some(copy_progress(&[
            ("type", Some("FILE")),
            ("tuples_processed", Some("12")),
            ("bytes_total", Some("100")),
            ("bytes_processed", Some("20")),
        ])),
    );
    let captured =
        reader::capture(&mut io, "fixture.invalid", 9, 42, None, &CollectionBudget::default(), reader::Mode::Live)
            .await
            .unwrap();
    assert_eq!(io.calls.len(), 11, "owned session setup is the twelfth and final admitted query");
    assert_eq!(MAX_LIVE_PROGRESS_QUERIES, 12);
    assert!(io.replies.is_empty());
    let reader::CapturedProgress::CurrentWork(snapshot) = &captured.progress else {
        panic!("COPY belongs to current work, not maintenance")
    };
    assert_eq!(snapshot.profile, CurrentRequestWorkProfile::PostgresCopy);
    assert_eq!(snapshot.result.membership, FocusedRequestMembership::Observed);
    let CurrentRequestWorkNative::PostgresCopy { progress: copy } = &snapshot.observations[0].native else { panic!() };
    assert_eq!(copy.bytes_processed.as_deref(), Some("20"));
    assert_eq!(copy.bytes_total.as_deref(), Some("100"));
    assert_eq!(copy.tuples_processed.as_deref(), Some("12"));
    let query = &io.calls[7];
    assert!(query.contains("pg_stat_progress_copy"));
    assert!(query.contains("a.backend_start=") && query.contains("a.query_start="));
    let progress = project("42", captured);
    assert!(progress.phase.is_none());
    assert!(progress.phase_percent.is_none());
    assert!(progress.percent.is_none());
    assert_eq!(progress.note.as_deref(), Some("COPY FROM FILE · 12 rows so far"));
    assert!(progress.current_work.as_ref().unwrap().is_valid());
    let mut no_io = Reader::default();
    let unavailable = copy::read(&mut no_io, "16384", "shop", &original_focus().original, 130_000, None).await.unwrap();
    assert!(unavailable.observation.is_none());
    assert_eq!(unavailable.unavailable, Some(culprit_core::ErrorCode::Unsupported));
    assert!(no_io.calls.is_empty());
}
