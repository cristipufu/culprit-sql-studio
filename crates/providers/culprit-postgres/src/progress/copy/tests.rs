use super::*;
use crate::progress::{self, source, test_support::*};
use culprit_core::{Error, SourceState};

async fn capture(io: &mut Reader, focus: &ActivityRequestFocus) -> Result<CurrentRequestWorkSnapshot> {
    match progress::reader::capture(
        io,
        "fixture.invalid",
        9,
        42,
        Some(focus),
        &progress::current_work_budget(&CollectionBudget::default()),
        progress::reader::Mode::CurrentWork,
    )
    .await?
    .progress
    {
        progress::reader::CapturedProgress::CurrentWork(snapshot) => Ok(snapshot),
        _ => panic!("a selected current-work read cannot return maintenance"),
    }
}

fn native(snapshot: &CurrentRequestWorkSnapshot) -> &PostgresCopyProgress {
    let CurrentRequestWorkNative::PostgresCopy { progress } = &snapshot.observations[0].native else { panic!() };
    progress
}

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::CurrentRequestWork { focus: Box::new(original_focus()) },
        budget: progress::current_work_budget(&CollectionBudget::default()),
    }
}

#[tokio::test]
async fn selected_copy_uses_one_native_view_and_exact_counts_without_a_phase_or_sql_reread() {
    for (version, skipped) in [(140_000, None), (170_000, Some("0")), (180_000, Some("12"))] {
        let mut io = progress_reader(
            version,
            0,
            Some(copy_progress(&[
                ("tuples_skipped", skipped),
                ("relation_oid", Some("0")),
                ("command", Some("COPY TO")),
            ])),
        );
        let input = capture(&mut io, &original_focus()).await.unwrap();
        assert!(io.replies.is_empty());
        assert_eq!(
            io.calls.len(),
            7,
            "six actor/context points plus one COPY view; shared ReadSession setup consumes the eighth query"
        );
        assert_eq!(progress::MAX_CURRENT_WORK_QUERIES, io.calls.len() as u32 + 1);
        assert_eq!(io.calls.iter().filter(|sql| sql.contains("FROM pg_catalog.pg_stat_progress_copy")).count(), 1);
        assert!(
            !io.calls
                .iter()
                .any(|sql| sql.contains("pg_stat_progress_vacuum") || sql.contains("pg_stat_progress_create_index"))
        );
        let query = &io.calls[3];
        assert!(
            query.contains("p.pid=42")
                && query.contains("p.datid=16384")
                && query.contains("a.backend_start=")
                && query.contains("a.query_start=")
        );
        assert!(query.contains("a.backend_type='client backend'") && query.ends_with("LIMIT 2"));
        assert!(query.contains("pg_catalog.to_jsonb(p)->>'tuples_skipped' AS tuples_skipped"));
        assert!(!query.contains("p.phase") && !io.calls[2].contains("current_sql"));
        let point = &input.observations[0];
        assert_eq!(point.actor, original_focus().original.key);
        assert_eq!(point.statement_identity, original_focus().statement_identity);
        assert_eq!(point.scope, ActivityScopeBasis::SessionDatabase);
        assert!(matches!(point.statement, DefinitionPayload::Unavailable { .. }));
        assert_eq!(native(&input).bytes_processed.as_deref(), Some("9007199254740993"));
        assert_eq!(native(&input).bytes_total.as_deref(), Some("0"));
        assert_eq!(native(&input).tuples_processed.as_deref(), Some("9007199254740993"));
        assert_eq!(native(&input).tuples_skipped.as_deref(), skipped);
        assert_eq!(native(&input).relation_oid, "0");
        assert_eq!(input.result.binding, FocusedRequestBinding::UnverifiedOriginalScope);
    }
}

#[tokio::test]
async fn older_versions_do_not_issue_an_unsupported_copy_query_or_claim_healthy_absence() {
    let mut io = progress_reader(130_000, 0, None);
    let input = capture(&mut io, &original_focus()).await.unwrap();
    assert_eq!(io.calls.len(), 6);
    assert!(io.replies.is_empty());
    assert!(!io.calls.iter().any(|sql| sql.contains("pg_stat_progress_copy")));
    assert!(input.observations.is_empty());
    assert_eq!(input.coverage.state, SourceState::Unavailable);
    assert_eq!(input.coverage.error_code, Some(ErrorCode::Unsupported));
    assert_eq!(input.result.membership, FocusedRequestMembership::Unavailable);
}

#[tokio::test]
async fn genuine_same_native_original_can_match_but_a_logical_origin_is_never_upgraded() {
    let mut io = progress_reader(180_000, 0, Some(copy_progress(&[])));
    let independent = capture(&mut io, &original_focus()).await.unwrap();
    let mut source = original_source();
    source.partition = independent.context.partition.clone();
    let SourceObservation::Activity(frame) = &mut source.observation else { panic!() };
    frame.epoch = independent.context.partition.native_epoch().map(str::to_owned);
    let focus = focus_from(&source, original_focus().original);
    let mut io = progress_reader(180_000, 0, Some(copy_progress(&[])));
    let matched = capture(&mut io, &focus).await.unwrap();
    assert_eq!(matched.result.binding, FocusedRequestBinding::Matched);
    assert!(matched.coverage.complete_scope);
    assert!(matched.is_valid());
    assert!(focus.is_current_request_work_focus());
}

#[tokio::test]
async fn optional_copy_denial_is_unavailable_but_terminal_stops_never_read_the_actor_or_owner_again() {
    let mut io = progress_reader(180_000, 0, Some(copy_progress(&[])));
    io.replies[3] = Err(Error::new(ErrorCode::PermissionDenied, "fixture denied"));
    let input = capture(&mut io, &original_focus()).await.unwrap();
    assert_eq!(input.coverage.state, SourceState::Unavailable);
    assert_eq!(input.coverage.error_code, Some(ErrorCode::PermissionDenied));
    assert_eq!(input.result.membership, FocusedRequestMembership::Unavailable);
    assert_eq!(io.calls.len(), 7);
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::QueryLimit,
        CollectionStop::RowLimit,
        CollectionStop::ByteLimit,
        CollectionStop::SourceChanged,
        CollectionStop::CancellationUnconfirmed,
    ] {
        let mut io = progress_reader(180_000, 0, Some(copy_progress(&[])));
        io.replies[3] = Err(stop.error());
        assert_eq!(CollectionStop::from_error(&capture(&mut io, &original_focus()).await.unwrap_err()), stop);
        assert_eq!(io.calls.len(), 4);
        assert_eq!(io.replies.len(), 3, "no fallback, actor or owner read follows a terminal stop");
    }
}

#[tokio::test]
async fn duplicate_foreign_or_malformed_copy_rows_never_gain_a_complete_source() {
    for changes in [
        vec![("pid", Some("43"))],
        vec![("backend_start", Some("2026-11-15T08:00:01Z"))],
        vec![("query_start", Some("2026-11-15T09:00:01Z"))],
        vec![("database_oid", Some("16385"))],
        vec![("bytes_processed", Some("-1"))],
        vec![("tuples_processed", Some("01"))],
        vec![("tuples_skipped", Some("18446744073709551616"))],
        vec![("command", Some("VACUUM"))],
        vec![("type", Some("UNKNOWN"))],
    ] {
        let mut io = progress_reader(180_000, 0, Some(copy_progress(&changes)));
        assert!(capture(&mut io, &original_focus()).await.is_err(), "{changes:?}");
        assert_eq!(io.calls.len(), 4);
    }
    let mut io = progress_reader(180_000, 0, Some(copy_progress(&[])));
    io.replies[3] = Ok(vec![copy_progress(&[]), copy_progress(&[])]);
    assert_eq!(
        CollectionStop::from_error(&capture(&mut io, &original_focus()).await.unwrap_err()),
        CollectionStop::InvalidPage
    );
    assert_eq!(io.calls.len(), 4);
}

#[tokio::test]
async fn final_request_backend_member_epoch_or_role_change_discards_all_copy_facts() {
    for changed in ["request", "backend", "member", "process", "role"] {
        let mut io = progress_reader(180_000, 0, Some(copy_progress(&[])));
        match changed {
            "request" => io.replies[4] = Ok(vec![actor(BACKEND, "2026-11-15T09:00:01Z")]),
            "backend" => io.replies[4] = Ok(vec![actor("2026-11-15T08:00:01Z", REQUEST)]),
            "member" => io.replies[6] = Ok(vec![member("127.0.0.2")]),
            "process" => io.replies[5] = Ok(vec![metadata_with("f", "2026-11-14T00:00:01Z")]),
            _ => io.replies[5] = Ok(vec![metadata_with("t", "2026-11-14T00:00:00Z")]),
        }
        assert_eq!(
            CollectionStop::from_error(&capture(&mut io, &original_focus()).await.unwrap_err()),
            CollectionStop::SourceChanged,
            "{changed}"
        );
        assert!(io.calls.len() <= 7);
    }
}

#[tokio::test]
async fn stricter_limits_and_close_ownership_are_preserved_through_the_existing_frozen_source() {
    let mut strict = request();
    strict.budget.max_queries = 7;
    assert_eq!(progress::current_work_budget(&strict.budget).max_queries, 7);
    let mut io = progress_reader(180_000, 0, Some(copy_progress(&[])));
    io.query_limit = Some(6); // One of the seven caller tasks is consumed by shared ReadSession setup.
    let mut source = source::fixture_source(io, &strict);
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::QueryLimit);
    let reads = source.query_count();
    assert!(source.finish().await.is_err());
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.query_count(), reads);
    let mut source = source::fixture_source(progress_reader(180_000, 0, Some(copy_progress(&[]))), &request());
    let descriptor = source.describe().await.unwrap();
    assert!(descriptor.matches_request(&request()));
    let reads = source.query_count();
    let CollectionRead::Page { records, next_cursor: None, .. } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(records.len(), 1);
    let TelemetryRecord::Operational(point) = &records[0] else { panic!() };
    let OperationalFact::CurrentRequestWork(point) = &point.fact else { panic!() };
    assert!(point.fits_source_scope(&descriptor));
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.query_count(), reads, "paging/finish/close do not reacquire the source");
    let mut io = progress_reader(180_000, 0, Some(copy_progress(&[])));
    io.unconfirmed = true;
    let mut source = source::fixture_source(io, &request());
    source.describe().await.unwrap();
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
}

#[tokio::test]
async fn live_copy_uses_the_same_typed_point_without_a_fake_phase_or_completion_bar() {
    let mut io = progress_reader(180_000, 4, Some(copy_progress(&[])));
    let captured = progress::reader::capture(
        &mut io,
        "fixture.invalid",
        9,
        42,
        None,
        &CollectionBudget::default(),
        progress::reader::Mode::Live,
    )
    .await
    .unwrap();
    assert_eq!(io.calls.len(), 11, "four existing maintenance views plus COPY; setup is the twelfth task");
    let live = progress::project("42", captured);
    assert_eq!(live.running, Some(true));
    assert!(live.current_work.as_ref().unwrap().is_valid());
    assert!(live.phase.is_none() && live.phase_percent.is_none() && live.percent.is_none());
    assert!(live.note.as_ref().is_some_and(|caption| caption.contains("9007199254740993")));
    assert!(live.request_key.is_some());
}
