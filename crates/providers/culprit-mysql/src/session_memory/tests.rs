use super::*;
use crate::{
    native::{error_log::context::tests as instance_rows, session_memory::tests as native_rows},
    read::test_support::Read,
    rows::Rec,
};
use culprit_core::SourceState;
use mysql_async::Value;

#[test]
fn scan_and_collection_share_native_ceilings_while_preserving_every_stricter_caller_field() {
    for requested in [
        CollectionBudget::default(),
        CollectionBudget {
            max_snapshot_rows: 10_000,
            page_rows: 1000,
            max_queries: 1000,
            page_bytes: 8 * 1024 * 1024,
            total_bytes: 1024 * 1024 * 1024,
            statement_timeout_ms: 60_000,
            total_timeout_ms: 120_000,
            ..Default::default()
        },
    ] {
        let actual = bounded_budget(&requested);
        actual.validate().unwrap();
        assert_eq!(actual.max_snapshot_rows, 128);
        assert_eq!(actual.page_rows, 128);
        assert_eq!(actual.max_queries, 24);
        assert_eq!(actual.page_bytes, 1024 * 1024);
        assert_eq!(actual.total_bytes, 8 * 1024 * 1024);
        assert_eq!(actual.statement_timeout_ms, 2000);
        assert_eq!(actual.total_timeout_ms, 10_000);
    }
    let strict = CollectionBudget {
        max_snapshot_rows: 1,
        page_rows: 1,
        max_queries: 1,
        page_bytes: 1024,
        total_bytes: 2048,
        max_sql_bytes: 1024,
        max_plan_bytes: 1024,
        max_pages: 1,
        statement_timeout_ms: 1,
        total_timeout_ms: 2,
    };
    strict.validate().unwrap();
    assert_eq!(bounded_budget(&strict), strict);
}

pub(crate) fn frame() -> Vec<Result<Vec<Rec>>> {
    let mut replies = native_rows::context_rows();
    replies.push(Ok(vec![native_rows::thread_row(120, Value::UInt(75), true, Value::UInt(4096))]));
    replies.push(Ok(vec![native_rows::event_row(120, 5, Value::NULL, Value::UInt(999), false)]));
    replies.extend(native_rows::context_rows_at("2026-11-13T10:00:00.123459Z"));
    replies
}

#[tokio::test]
async fn default_and_native_unsupported_profiles_open_no_session_and_return_explicit_coverage() {
    for (requested, availability, expected) in [
        (false, native_memory::Availability::Available, SourceState::NotApplicable),
        (true, native_memory::Availability::UnsupportedProduct, SourceState::NotApplicable),
        (true, native_memory::Availability::UnsupportedVersion, SourceState::NotApplicable),
        (true, native_memory::Availability::InstrumentationDisabled, SourceState::Unavailable),
    ] {
        let mut reader = Read::default();
        let (snapshot, coverage) = collect_from(&mut reader, requested, availability).await;
        assert!(snapshot.is_none());
        assert_eq!(coverage.outcomes()[0].state, expected);
        assert!(reader.queries.is_empty());
        assert_eq!(reader.closed, 0);
    }
}

#[tokio::test]
async fn scan_projection_preserves_shared_native_identity_exact_allocations_and_independent_context() {
    let mut reader = Read::with(frame());
    let (snapshot, coverage) = collect_from(&mut reader, true, native_memory::Availability::Available).await;
    let snapshot = snapshot.unwrap();
    assert!(snapshot.is_valid());
    assert_eq!(snapshot.resource, instance_rows::resource());
    let row = &snapshot.observations[0];
    assert_eq!(row.identity.id, "120");
    assert_eq!(row.identity.qualifiers["server_uuid"], instance_rows::UUID);
    assert_eq!(row.identity.qualifiers["server_started_second"], "9900");
    assert_eq!(row.session.id, "75");
    assert_eq!(row.session.generation, "120");
    assert_eq!(row.current_bytes.as_deref(), Some("4096"));
    assert_eq!(row.controlled_current_bytes.as_deref(), Some("0"));
    assert_eq!(row.high_water_bytes.as_deref(), Some("18446744073709551615"));
    assert!(row.tracking_enabled);
    let event = row.current_context.as_ref().unwrap();
    assert_eq!(event.request.id, "5");
    assert_ne!(event.observed_at, row.observed_at);
    assert_eq!(row.native_properties["INSTRUMENTED"], "YES");
    assert_eq!(coverage.outcomes()[0].state, SourceState::Collected);
    assert_eq!(reader.queries.len(), 10);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn only_confirmed_shutdown_can_release_provisional_scan_evidence() {
    let mut reader = Read::with(frame());
    reader.shutdown = Some(SourceShutdown::Unconfirmed);
    let (snapshot, coverage) = collect_from(&mut reader, true, native_memory::Availability::Available).await;
    assert!(snapshot.is_none());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Unavailable);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn expected_resource_is_checked_before_data_and_final_native_identity_changes_discard_rows() {
    let mut other = instance_rows::resource();
    other.native.id = "f54a461c-0001-4000-8000-000000000002".into();
    let mut reader = Read::with(native_rows::context_rows());
    assert_eq!(
        capture(&mut reader, 2, Some(&other)).await.err().unwrap().detail,
        CollectionStop::SourceChanged.error().detail
    );
    assert_eq!(reader.queries.len(), 4);
    for boundary in [6, 7, 8, 9] {
        let mut replies = frame();
        replies[boundary] = match boundary {
            6 => Ok(vec![instance_rows::platform("8.4.1", true)]),
            7 => Ok(vec![instance_rows::configuration("f54a461c-0001-4000-8000-000000000002", "")]),
            8 => Ok(vec![instance_rows::process(10_000, 99)]),
            _ => Ok(native_rows::consumer_rows(false, true, true)),
        };
        let mut reader = Read::with(replies);
        let (snapshot, coverage) = collect_from(&mut reader, true, native_memory::Availability::Available).await;
        assert!(snapshot.is_none(), "source context boundary {boundary}");
        assert_eq!(coverage.outcomes()[0].state, SourceState::Unavailable);
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn only_the_verified_data_connection_is_excluded_and_unqualified_numeric_ids_remain() {
    let mut replies = frame();
    replies[4] = Ok(vec![
        native_rows::thread_row(120, Value::UInt(75), true, Value::UInt(4096)),
        native_rows::thread_row(121, Value::UInt(76), true, Value::UInt(8192)),
        native_rows::thread_row(122, Value::UInt(55), true, Value::UInt(16384)),
    ]);
    let mut reader = Read::with(replies);
    let captured = capture(&mut reader, 3, None).await.unwrap();
    assert_eq!(
        captured.snapshot.observations.iter().map(|row| row.session.id.as_str()).collect::<Vec<_>>(),
        ["75", "76"]
    );
    assert!(reader.queries.iter().all(|(sql, _, _)| !sql.contains("session_connect_attrs")));
}

#[tokio::test]
async fn disabled_native_tracking_is_reported_even_when_no_foreground_session_was_retained() {
    let mut replies = native_rows::context_rows();
    replies[3] = Ok(native_rows::consumer_rows(false, true, true));
    replies.push(Ok(vec![]));
    let mut after = native_rows::context_rows_at("2026-11-13T10:00:00.123459Z");
    after[3] = Ok(native_rows::consumer_rows(false, true, true));
    replies.extend(after);
    let mut reader = Read::with(replies);
    let (snapshot, coverage) = collect_from(&mut reader, true, native_memory::Availability::Available).await;
    assert!(snapshot.unwrap().observations.is_empty());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
    assert_eq!(
        coverage.outcomes()[0].reason,
        Some(culprit_core::t!("provider.mysql.session_memory.tracking_disabled"))
    );
    assert_eq!(reader.queries.len(), 9);
}

#[tokio::test]
async fn missing_statement_permission_preserves_session_facts_as_partial_without_fabricating_context() {
    let mut replies = frame();
    replies[5] = Err(Error::new(ErrorCode::PermissionDenied, "native event source denied"));
    let mut reader = Read::with(replies);
    let (snapshot, coverage) = collect_from(&mut reader, true, native_memory::Availability::Available).await;
    let snapshot = snapshot.unwrap();
    assert_eq!(snapshot.observations[0].current_bytes.as_deref(), Some("4096"));
    assert!(snapshot.observations[0].current_context.is_none());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
    assert_eq!(reader.queries.len(), 10, "a drained permission error allows native identity verification");
    assert_eq!(reader.closed, 1);
    let (_, _, notes) = coverage.finish();
    assert!(notes.iter().any(|note| note.message.contains("native event source denied")));
}

#[tokio::test]
async fn terminal_read_failures_stop_at_each_boundary_without_followup_metadata_or_fallbacks() {
    for boundary in 0..10 {
        let mut replies = frame();
        replies[boundary] = Err(CollectionStop::CancellationUnconfirmed.error());
        let mut reader = Read::with(replies);
        let (snapshot, coverage) = collect_from(&mut reader, true, native_memory::Availability::Available).await;
        assert!(snapshot.is_none(), "source boundary {boundary}");
        assert_eq!(coverage.outcomes()[0].state, SourceState::Unavailable);
        assert_eq!(reader.queries.len(), boundary + 1);
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn a_native_limit_witness_stays_partial_while_a_drained_empty_population_is_empty_at_capture_only() {
    let mut replies = frame();
    replies[4] = Ok((0..=native_memory::MAX_ROWS)
        .map(|index| native_rows::thread_row(index as u64 + 120, Value::UInt(index as u64 + 75), true, Value::UInt(1)))
        .collect());
    let mut reader = Read::with(replies);
    let (snapshot, coverage) = collect_from(&mut reader, true, native_memory::Availability::Available).await;
    assert_eq!(snapshot.unwrap().observations.len(), native_memory::MAX_ROWS);
    assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
    let mut replies = native_rows::context_rows();
    replies.push(Ok(vec![]));
    replies.extend(native_rows::context_rows());
    let mut reader = Read::with(replies);
    let (snapshot, coverage) = collect_from(&mut reader, true, native_memory::Availability::Available).await;
    assert!(snapshot.unwrap().observations.is_empty());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Empty);
    assert_eq!(reader.queries.len(), 9);
}
