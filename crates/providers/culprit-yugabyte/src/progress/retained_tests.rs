use super::{test_support::*, *};

async fn capture(io: &mut Reader, focus: &ActivityRequestFocus) -> Result<MaintenanceProgressSnapshot> {
    reader::capture(io, "fixture.invalid", 9, focus, &bounded_budget(&CollectionBudget::default())).await
}

#[tokio::test]
async fn local_ysql_progress_retains_exact_backfill_work_and_unknown_original_scope_in_nine_native_phases() {
    let mut io = scripted_reader();
    let point = capture(&mut io, &original_focus()).await.unwrap();
    assert_eq!(io.calls.len(), 9);
    assert_eq!(io.calls.iter().filter(|sql| sql.contains("FROM pg_catalog.pg_stat_progress_create_index")).count(), 1);
    assert!(!io.calls[4].contains("gv$"));
    assert!(!io.calls[4].contains("blocks_done"));
    assert_eq!(point.result.binding, FocusedRequestBinding::UnverifiedOriginalScope);
    let MaintenanceProgressWork::YsqlIndexBuild { progress } = &point.observations[0].work else { panic!() };
    assert_eq!(progress.tuples_done.as_deref(), Some("9007199254740993"));
    assert_eq!(progress.tuples_total_estimate.as_deref(), Some("42"));
    assert!(point.is_valid(), "native estimate may be lower than work already observed");
}

#[tokio::test]
async fn exact_original_local_member_and_generation_can_match_but_remote_activity_never_borrows_local_epoch() {
    let mut io = scripted_reader();
    let captured = capture(&mut io, &original_focus()).await.unwrap();
    let mut source = original_source();
    source.partition = captured.context.partition.clone();
    let SourceObservation::Activity(frame) = &mut source.observation else { panic!() };
    frame.epoch = captured.context.partition.native_epoch().map(str::to_owned);
    let focus = focus_from(&source, original_focus().original);
    let mut io = scripted_reader();
    assert_eq!(capture(&mut io, &focus).await.unwrap().result.binding, FocusedRequestBinding::Matched);
    let mut io = scripted_reader();
    io.replies[1] = Ok(vec![node("1123456789abcdef0123456789abcdef")]);
    assert_eq!(
        CollectionStop::from_error(&capture(&mut io, &original_focus()).await.unwrap_err()),
        CollectionStop::SourceUnavailable
    );
    assert_eq!(io.calls.len(), 3);
    assert!(!io.calls.iter().any(|sql| sql.contains("pg_stat_progress")));
}

#[tokio::test]
async fn switched_request_backend_node_process_or_control_connection_never_publish_stale_progress() {
    for change in ["request", "backend", "node", "process", "connection", "cancel", "timeout", "budget"] {
        let mut io = scripted_reader();
        match change {
            "request" => io.replies[5] = Ok(vec![actor(BACKEND, "2026-11-15T09:00:01Z")]),
            "backend" => io.replies[5] = Ok(vec![actor("2026-11-15T08:00:01Z", REQUEST)]),
            "node" => io.replies[7] = Ok(vec![node("1123456789abcdef0123456789abcdef")]),
            "process" => io.replies[6] = Ok(vec![metadata("2026-11-14T00:00:01Z")]),
            "connection" => io.replies[8] = Ok(vec![row(&[("backend", Some("100"))])]),
            "cancel" => io.replies[4] = Err(CollectionStop::Cancelled.error()),
            "timeout" => io.replies[4] = Err(CollectionStop::TimedOut.error()),
            _ => io.replies[4] = Err(CollectionStop::QueryLimit.error()),
        }
        assert!(capture(&mut io, &original_focus()).await.is_err(), "{change}");
        assert!(io.calls.len() <= 9);
    }
}

#[tokio::test]
async fn progress_permission_and_native_nulls_remain_unknown_not_completed_or_zero() {
    let mut io = scripted_reader();
    io.replies[4] = Err(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denied"));
    let snapshot = capture(&mut io, &original_focus()).await.unwrap();
    assert_eq!(snapshot.result.membership, FocusedRequestMembership::Unavailable);
    assert_eq!(snapshot.coverage.state, culprit_core::SourceState::Unavailable);
    let native = row(&[("tuples_done", None), ("tuples_total", None)]);
    assert_eq!(index::work(&native).unwrap(), YsqlIndexBuildProgress::default());
    assert!(index::work(&row(&[("tuples_done", Some("0"))])).is_err());
    assert!(index::work(&row(&[("tuples_done", Some("-1")), ("tuples_total", None)])).is_err());
}

#[tokio::test]
async fn changed_or_missing_original_skips_native_progress_without_an_outcome_verdict() {
    for missing in [false, true] {
        let mut io = scripted_reader();
        io.replies.remove(4);
        let actor = if missing {
            row(&[("observed_at", Some(AT)), ("pid", None)])
        } else {
            actor(BACKEND, "2026-11-15T09:00:01Z")
        };
        io.replies[3] = Ok(vec![actor.clone()]);
        io.replies[4] = Ok(vec![actor]);
        let snapshot = capture(&mut io, &original_focus()).await.unwrap();
        assert_eq!(snapshot.result.membership, FocusedRequestMembership::NotObserved);
        assert!(snapshot.observations.is_empty());
        assert!(!io.calls.iter().any(|sql| sql.contains("pg_stat_progress_create_index")));
    }
}
