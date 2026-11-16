use super::{test_support::*, *};

async fn capture(reader: &mut Reader, original: &ActivityRequestFocus) -> Result<MaintenanceProgressSnapshot> {
    capture_from(reader, "fixture.invalid", 9, "42", Some(original), &bounded_budget(&CollectionBudget::default()))
        .await
}

#[tokio::test]
async fn one_existing_progress_read_retains_exact_phase_counts_without_promoting_unknown_original_scope() {
    let mut reader = scripted_reader();
    let snapshot = capture(&mut reader, &original_focus()).await.unwrap();
    assert_eq!(reader.calls.len(), 7);
    assert_eq!(
        reader.calls.iter().filter(|sql| sql.contains("FROM pg_catalog.pg_stat_progress_create_index")).count(),
        1
    );
    assert_eq!(snapshot.result.binding, FocusedRequestBinding::UnverifiedOriginalScope);
    assert_eq!(snapshot.result.membership, FocusedRequestMembership::Observed);
    assert!(!reader.calls[2].contains("current_sql"), "retained progress does not reread SQL bodies");
    let point = &snapshot.observations[0];
    let MaintenanceProgressWork::PostgreSqlIndexBuild { progress } = &point.work else { panic!() };
    assert_eq!(progress.blocks_total.as_deref(), Some("9007199254740993"));
    assert_eq!(progress.blocks_done.as_deref(), Some("0"));
    assert!(point.index_reference().is_none());
    assert!(reader.calls[3].contains("a.backend_start=") && reader.calls[3].contains("a.query_start="));
    assert!(reader.calls[3].contains("p.datid=16384") && reader.calls[3].ends_with("LIMIT 2"));
}

#[tokio::test]
async fn exact_qualified_original_and_current_owner_can_match_without_borrowing_another_frame() {
    let mut first = scripted_reader();
    let captured = capture(&mut first, &original_focus()).await.unwrap();
    let mut source = original_source();
    source.partition = captured.context.partition.clone();
    let SourceObservation::Activity(frame) = &mut source.observation else { panic!() };
    frame.epoch = captured.context.partition.native_epoch().map(str::to_owned);
    let source_focus = focus_from(&source, original_focus().original);
    let mut next = scripted_reader();
    let snapshot = capture(&mut next, &source_focus).await.unwrap();
    assert_eq!(snapshot.result.binding, FocusedRequestBinding::Matched);
    assert!(snapshot.is_valid());
}

#[tokio::test]
async fn changed_actor_member_role_and_process_or_terminal_stops_never_publish_progress() {
    for change in ["request", "backend", "member", "process", "role", "cancel", "timeout", "budget"] {
        let mut reader = scripted_reader();
        match change {
            "request" => reader.replies[4] = Ok(vec![actor(BACKEND, "2026-11-15T09:00:01Z")]),
            "backend" => reader.replies[4] = Ok(vec![actor("2026-11-15T08:00:01Z", REQUEST)]),
            "member" => reader.replies[6] = Ok(vec![member("127.0.0.2")]),
            "process" | "role" => {
                reader.replies[5] = Ok(vec![if change == "process" {
                    metadata_with("f", "2026-11-14T00:00:01Z")
                } else {
                    metadata_with("t", "2026-11-14T00:00:00Z")
                }]);
            }
            "cancel" => reader.replies[3] = Err(CollectionStop::Cancelled.error()),
            "timeout" => reader.replies[3] = Err(CollectionStop::TimedOut.error()),
            _ => reader.replies[3] = Err(CollectionStop::QueryLimit.error()),
        }
        assert!(capture(&mut reader, &original_focus()).await.is_err(), "{change}");
        assert!(reader.calls.len() <= 7);
    }
}

#[tokio::test]
async fn missing_or_recycled_original_skips_the_progress_view_and_never_reports_completed() {
    for missing in [true, false] {
        let mut reader = scripted_reader();
        reader.replies.remove(3);
        if missing {
            reader.replies[2] = Ok(vec![row(&[("observed_at", Some(AT)), ("pid", None)])]);
            reader.replies[3] = Ok(vec![row(&[("observed_at", Some(AT)), ("pid", None)])]);
        } else {
            reader.replies[2] = Ok(vec![actor(BACKEND, "2026-11-15T09:00:01Z")]);
            reader.replies[3] = Ok(vec![actor(BACKEND, "2026-11-15T09:00:01Z")]);
        }
        let snapshot = capture(&mut reader, &original_focus()).await.unwrap();
        assert!(snapshot.observations.is_empty());
        assert_eq!(snapshot.result.membership, FocusedRequestMembership::NotObserved);
        assert!(!reader.calls.iter().any(|sql| sql.contains("pg_stat_progress_create_index")));
    }
}

#[tokio::test]
async fn permission_denial_is_unavailable_while_native_null_and_ambiguous_rows_are_not_zero() {
    let mut reader = progress_reader(180_000, 4, None);
    reader.replies[3] = Err(Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denied"));
    let snapshot = capture(&mut reader, &original_focus()).await.unwrap();
    assert_eq!(snapshot.coverage.state, culprit_core::SourceState::Unavailable);
    assert_eq!(snapshot.result.membership, FocusedRequestMembership::Unavailable);
    let mut reader = scripted_reader();
    reader.replies[3] = Ok(vec![progress(), progress()]);
    assert_eq!(
        CollectionStop::from_error(&capture(&mut reader, &original_focus()).await.unwrap_err()),
        CollectionStop::InvalidPage
    );
}

#[tokio::test]
async fn malformed_or_different_session_selector_is_refused_before_any_source_work() {
    let mut reader = Reader::default();
    for session in ["0", "2147483648", "not-a-pid", "43"] {
        assert!(
            capture_from(
                &mut reader,
                "fixture.invalid",
                9,
                session,
                Some(&original_focus()),
                &CollectionBudget::default()
            )
            .await
            .is_err()
        );
        assert!(reader.calls.is_empty());
    }
}

#[tokio::test]
async fn legacy_progress_skips_views_not_present_in_that_native_version_and_preserves_terminal_errors() {
    let actor = original_focus().original.key;
    for (version, views) in [(110_000, 1), (120_000, 3), (130_000, 4), (140_000, 4)] {
        let mut io = Reader { replies: (0..views).map(|_| Ok(vec![])).collect(), ..Default::default() };
        assert!(maintenance::read(&mut io, "16384", &actor, version).await.unwrap().observation.is_none());
        assert_eq!(io.calls.len(), views);
        assert_eq!(io.calls.iter().any(|sql| sql.contains("pg_stat_progress_analyze")), version >= 130_000);
        assert_eq!(io.calls.iter().any(|sql| sql.contains("pg_stat_progress_cluster")), version >= 120_000);
        assert!(!io.calls.iter().any(|sql| sql.contains("pg_stat_progress_copy")));
    }
    for stopped in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::QueryLimit] {
        let mut io = Reader { replies: [Err(stopped.error())].into(), ..Default::default() };
        assert_eq!(
            CollectionStop::from_error(&maintenance::read(&mut io, "16384", &actor, 180_000).await.err().unwrap()),
            stopped
        );
        assert_eq!(io.calls.len(), 1);
    }
}

#[tokio::test]
async fn postgres_11_live_capture_uses_one_compatible_clock_projection_and_only_available_progress_views() {
    let mut io = scripted_reader();
    io.replies[0] = Ok(vec![metadata_on_version(110_000, "f", "2026-11-14T00:00:00Z")]);
    io.replies[3] = Ok(vec![vacuum_progress(110_000, &[("heap_blks_total", Some("10"))])]);
    io.replies[5] = Ok(vec![metadata_on_version(110_000, "f", "2026-11-14T00:00:00Z")]);
    let captured =
        reader::capture(&mut io, "fixture.invalid", 9, 42, None, &CollectionBudget::default(), reader::Mode::Live)
            .await
            .unwrap();
    assert_eq!(io.calls.len(), 7);
    assert!(io.replies.is_empty());
    let actors = io.calls.iter().filter(|sql| sql.starts_with("WITH stamp AS ")).collect::<Vec<_>>();
    assert_eq!(actors.len(), 2, "the original actor is rechecked through the same shared projection");
    for sql in actors {
        assert!(sql.starts_with("WITH stamp AS (SELECT pg_catalog.clock_timestamp() AS at)"));
        assert_eq!(sql.matches("pg_catalog.clock_timestamp()").count(), 1);
        assert!(!sql.contains("MATERIALIZED"));
        assert!(sql.contains("FROM stamp LEFT JOIN LATERAL"));
        assert!(sql.contains("WHERE pid=42"));
        assert!(sql.contains("NULL::text AS query_id"));
    }
    assert_eq!(io.calls.iter().filter(|sql| sql.contains("pg_stat_progress_vacuum")).count(), 1);
    for absent in [
        "pg_stat_progress_create_index",
        "pg_stat_progress_cluster",
        "pg_stat_progress_analyze",
        "pg_stat_progress_copy",
    ] {
        assert!(!io.calls.iter().any(|sql| sql.contains(absent)), "{absent}");
    }
    assert_eq!(captured.actor.as_ref().unwrap().key, original_focus().original.key);
    assert!(captured.snapshot.context.has_qualified_local_owner());
    assert!(captured.other_phase.is_none());
    assert_eq!(captured.snapshot.source, vacuum::SOURCE);
    assert_eq!(phase_fraction(&captured.snapshot.observations[0]), Some(0.2));
    assert_eq!(captured.snapshot.result.membership, FocusedRequestMembership::Observed);
    assert_eq!(captured.snapshot.coverage.state, culprit_core::SourceState::Partial);
    assert!(captured.snapshot.is_valid());
    let progress = project("42", captured);
    assert_eq!(progress.phase.as_deref(), Some("VACUUM: scanning heap"));
    assert_eq!(progress.phase_percent, Some(0.2));
    assert_eq!(progress.percent, None, "one phase never establishes whole-command completion");
}
