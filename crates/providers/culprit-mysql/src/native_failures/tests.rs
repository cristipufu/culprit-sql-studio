use super::*;
use crate::native::error_log::{
    context::tests::{self as context_rows, UUID},
    tests::{AT, row},
};
use crate::read::test_support::Read;
use culprit_core::{SourceState, telemetry::SourceShutdown};
use mysql_async::Value;

pub(crate) fn frame(rows: Vec<crate::rows::Rec>) -> Vec<Result<Vec<crate::rows::Rec>>> {
    let mut replies = context_rows::rows();
    replies.push(Ok(rows));
    replies.extend(context_rows::rows());
    replies
}

fn rows(count: usize) -> Vec<crate::rows::Rec> {
    (0..count).map(|_| row(AT, Value::UInt(55), Value::from("MY-010935"))).collect()
}

#[tokio::test]
async fn default_and_unsupported_profiles_execute_no_source_queries() {
    for (requested, availability, state) in [
        (false, error_log::Availability::Available, SourceState::NotApplicable),
        (true, error_log::Availability::UnsupportedProduct, SourceState::NotApplicable),
        (true, error_log::Availability::UnsupportedVersion, SourceState::NotApplicable),
        (true, error_log::Availability::InstrumentationDisabled, SourceState::Unavailable),
    ] {
        let mut reader = Read::default();
        let (snapshot, coverage) = collect_from(&mut reader, requested, availability).await;
        assert!(snapshot.is_none());
        assert_eq!(coverage.outcomes()[0].state, state);
        assert!(reader.queries.is_empty());
        assert_eq!(reader.closed, 0, "no native session was opened");
    }
}

#[tokio::test]
async fn completed_scan_requires_confirmed_close_and_preserves_duplicate_rows_and_native_metadata() {
    let mut reader = Read::with(frame(rows(2)));
    let (snapshot, coverage) = collect_from(&mut reader, true, error_log::Availability::Available).await;
    let snapshot = snapshot.unwrap();
    assert!(snapshot.is_valid());
    assert_eq!(snapshot.resource, Some(context_rows::resource()));
    assert_eq!(snapshot.observations.len(), 2);
    assert_eq!(reader.queries.len(), 7);
    assert_eq!(reader.closed, 1);
    assert_eq!(coverage.outcomes()[0].state, SourceState::Collected);
    for (position, native) in snapshot.observations.iter().enumerate() {
        let expected = error_log::decode(&row(AT, Value::UInt(55), Value::from("MY-010935")), position as u32).unwrap();
        assert_eq!(native, &NativeFailureRecord::MysqlErrorLog(expected));
    }
    let mut reader = Read::with(frame(rows(2)));
    reader.shutdown = Some(SourceShutdown::Unconfirmed);
    let (snapshot, coverage) = collect_from(&mut reader, true, error_log::Availability::Available).await;
    assert!(snapshot.is_none(), "unconfirmed shutdown discards all provisional observations");
    assert_eq!(coverage.outcomes()[0].state, SourceState::Unavailable);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn a_limit_witness_is_partial_while_a_drained_empty_buffer_is_only_empty_at_read_time() {
    let mut reader = Read::with(frame(rows(error_log::MAX_ROWS + 1)));
    let (snapshot, coverage) = collect_from(&mut reader, true, error_log::Availability::Available).await;
    assert_eq!(snapshot.unwrap().observations.len(), error_log::MAX_ROWS);
    assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
    let mut reader = Read::with(frame(vec![]));
    let (snapshot, coverage) = collect_from(&mut reader, true, error_log::Availability::Available).await;
    assert!(snapshot.unwrap().observations.is_empty());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Empty);
}

#[tokio::test]
async fn mismatched_requested_owner_fails_before_data_sql_and_changed_final_context_discards_rows() {
    let mut other = context_rows::resource();
    other.native.id = "f54a461c-0001-4000-8000-000000000002".into();
    let mut reader = Read::with(context_rows::rows());
    assert_eq!(
        capture(&mut reader, 2, Some(&other)).await.err().unwrap().detail,
        CollectionStop::SourceChanged.error().detail
    );
    assert_eq!(reader.queries.len(), 3);
    for index in [4, 5, 6] {
        let mut replies = frame(rows(1));
        replies[index] = match index {
            4 => Ok(vec![context_rows::platform("8.4.1", true)]),
            5 => Ok(vec![context_rows::configuration(UUID, "log_sink_json")]),
            _ => Ok(vec![context_rows::process(10_000, 99)]),
        };
        let mut reader = Read::with(replies);
        let (snapshot, coverage) = collect_from(&mut reader, true, error_log::Availability::Available).await;
        assert!(snapshot.is_none(), "context field at source read {index} changed");
        assert_eq!(coverage.outcomes()[0].state, SourceState::Unavailable);
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn terminal_data_reads_stop_before_final_metadata_and_only_confirmed_source_permissions_get_guidance() {
    for error in [
        CollectionStop::TimedOut.error(),
        CollectionStop::CancellationUnconfirmed.error(),
        CollectionStop::ByteLimit.error(),
        Error::new(ErrorCode::PermissionDenied, "native permission denied"),
    ] {
        let permission = error.code == ErrorCode::PermissionDenied && error.detail.is_none();
        let mut replies = context_rows::rows();
        replies.push(Err(error));
        let mut reader = Read::with(replies);
        let (snapshot, coverage) = collect_from(&mut reader, true, error_log::Availability::Available).await;
        assert!(snapshot.is_none());
        assert_eq!(reader.queries.len(), 4, "a failed data read cannot be followed by metadata or fallback SQL");
        assert_eq!(reader.closed, 1);
        let (_, _, notes) = coverage.finish();
        assert_eq!(
            notes.iter().any(|note| note.message.contains("SELECT on performance_schema.error_log")),
            permission
        );
    }
}

#[tokio::test]
async fn cancellation_at_any_native_boundary_stops_further_sql_and_retires_the_owned_reader() {
    for boundary in 0..7 {
        let mut replies = frame(rows(1));
        replies[boundary] = Err(CollectionStop::CancellationUnconfirmed.error());
        let mut reader = Read::with(replies);
        let (snapshot, coverage) = collect_from(&mut reader, true, error_log::Availability::Available).await;
        assert!(snapshot.is_none(), "read boundary {boundary}");
        assert_eq!(reader.queries.len(), boundary + 1, "no SQL after cancellation at boundary {boundary}");
        assert_eq!(reader.closed, 1);
        assert_eq!(coverage.outcomes()[0].state, SourceState::Unavailable);
    }
}

#[test]
fn persisted_source_availability_and_permission_guidance_reword_in_the_readers_language() {
    let (reason, hint) = culprit_core::i18n::scope("en", || {
        let reason = context::unavailable(error_log::Availability::InstrumentationDisabled).message_source();
        let hint = source_error(Error::new(ErrorCode::PermissionDenied, "native permission")).hint_source().unwrap();
        (reason, hint)
    });
    let english = (reason.render(), hint.render());
    culprit_core::i18n::scope("fr", || {
        assert_eq!(reason.render(), culprit_core::t!("provider.mysql.native_failures.instrumentation"));
        assert_eq!(hint.render(), culprit_core::t!("provider.mysql.native_failures.permissions"));
        assert_ne!(reason.render(), english.0);
        assert_ne!(hint.render(), english.1);
    });
}
