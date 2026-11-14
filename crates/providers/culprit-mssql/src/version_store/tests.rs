use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

pub(crate) fn row() -> Rec {
    Rec::of(&[("native_rows", Val::Int(1)), ("known_rows", Val::Int(1)), ("reserved_space_kb", Val::Int(0))])
}
#[tokio::test]
async fn tempdb_store_keeps_native_integer_kibibytes_and_projects_bytes_only_for_display() {
    let mut reader =
        FakeRead::new(vec![Ok(vec![replace(&row(), &[("reserved_space_kb", Val::Int(9_007_199_254_740_993))])])]);
    let sample = read(&mut reader).await.unwrap();
    assert_eq!(sample.value, culprit_core::telemetry::MeasurementValue::Integer("9007199254740993".into()));
    assert_eq!(sample.unit, CounterUnit::Kibibytes);
    assert_eq!(sample.nature, CounterNature::Gauge);
    assert_eq!(sample.scope, MeasurementScope::Database);
    assert_eq!(sample.native_object, "sys.dm_tran_version_store_space_usage");
    assert!(sample.is_valid());
    assert!(observation(sample).is_valid());
    let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[("reserved_space_kb", Val::Int(42))])])]);
    assert_eq!(bytes(&read(&mut reader).await.unwrap()), Some(42.0 * 1024.0));
}
#[tokio::test]
async fn an_empty_visible_store_is_zero_but_unknown_contributors_or_missing_results_are_not() {
    let empty =
        replace(&row(), &[("native_rows", Val::Int(0)), ("known_rows", Val::Int(0)), ("reserved_space_kb", Val::Null)]);
    let mut reader = FakeRead::new(vec![Ok(vec![empty])]);
    assert_eq!(read(&mut reader).await.unwrap().value, culprit_core::telemetry::MeasurementValue::Integer("0".into()));
    for result in [
        Ok(vec![]),
        Ok(vec![row(), row()]),
        Ok(vec![replace(&row(), &[("known_rows", Val::Int(0))])]),
        Ok(vec![replace(&row(), &[("native_rows", Val::Int(0)), ("known_rows", Val::Int(0))])]),
        Ok(vec![replace(&row(), &[("reserved_space_kb", Val::Null)])]),
        Ok(vec![replace(&row(), &[("reserved_space_kb", Val::Int(-1))])]),
        Err(ReadError::Stopped(CollectionStop::TimedOut)),
    ] {
        let mut reader = FakeRead::new(vec![result]);
        assert!(read(&mut reader).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
}
#[tokio::test]
async fn unknown_meaning_or_scope_cannot_be_projected_as_database_store_bytes() {
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let sample = read(&mut reader).await.unwrap();
    for changed in [
        NativeCounterSample { unit: CounterUnit::Unknown, ..sample.clone() },
        NativeCounterSample { nature: CounterNature::Unknown, unit: CounterUnit::Unknown, ..sample.clone() },
        NativeCounterSample { scope: MeasurementScope::Server, ..sample.clone() },
    ] {
        assert!(bytes(&changed).is_none());
    }
}

#[test]
fn tempdb_context_cannot_measure_adr_versions_or_an_unknown_recovery_configuration() {
    for adr in [None, Some(true), Some(false)] {
        let context = StoreContext { tempdb_bytes: Some(1024.0), persistent: None, adr_enabled: adr };
        assert_eq!(context.tempdb_cleanup_context(), if adr == Some(false) { Some(1024.0) } else { None });
    }
}

fn application_reader(tempdb: crate::read::ReadResult<Vec<Rec>>) -> FakeRead {
    FakeRead::new(vec![
        tempdb,
        Ok(vec![persistent::tests::layout(3, 16, 1, 1)]),
        Ok(vec![Rec::of(&[("adr", Val::Int(1))])]),
        Ok(vec![persistent::tests::row()]),
    ])
}

#[tokio::test]
async fn scan_retains_original_pvs_facts_and_its_own_read_bounds_after_confirmed_shutdown() {
    let mut reader = application_reader(Ok(vec![row()]));
    let before = chrono::Utc::now();
    let mut coverage = Coverage::default();
    let context = application_with(&mut reader, &mut coverage, "test grant").await;
    let after = chrono::Utc::now();
    assert_eq!(reader.closed, 1);
    assert_eq!(reader.calls.len(), 4);
    assert_eq!(context.tempdb_bytes, Some(0.0));
    assert_eq!(context.tempdb_cleanup_context(), None, "ADR PVS is not measured by tempdb bytes");
    assert!(context.persistent_signal().is_some());
    let snapshot = context.persistent.unwrap();
    assert!(snapshot.is_valid());
    assert_eq!(snapshot.source, crate::sources::PERSISTENT_VERSION_STORE.name);
    assert_eq!(snapshot.observations.len(), 1);
    let start = chrono::DateTime::parse_from_rfc3339(&snapshot.observed_from).unwrap();
    let end = chrono::DateTime::parse_from_rfc3339(&snapshot.observed_until).unwrap();
    assert!(before <= start && start <= end && end <= after);
    let OperationalFact::MssqlPersistentVersionStore(sample) = &snapshot.observations[0].fact else { panic!() };
    assert_eq!(sample.off_row_kibibytes.as_deref(), Some("9007199254740993"));
    assert_eq!(sample.minimum_transaction_timestamp.as_deref(), Some("-1"));
    assert_eq!(sample.off_row_cleanup.started_without_offset.as_deref(), Some("2020-01-01T07:58:30.1533333"));
    assert!(coverage.outcomes().iter().all(|outcome| outcome.state == culprit_core::SourceState::Collected));
}

#[tokio::test]
async fn independent_source_failures_preserve_other_confirmed_measurements_without_fabricating_a_pvs_size() {
    let denied = || culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "test denied");
    let mut io = application_reader(Err(ReadError::Source(denied())));
    let mut coverage = Coverage::default();
    let context = application_with(&mut io, &mut coverage, "test grant").await;
    assert_eq!(context.tempdb_bytes, None);
    assert!(context.persistent.is_some());
    assert_eq!(io.calls.len(), 4);
    assert_eq!(io.closed, 1);
    assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::Unavailable);
    assert_eq!(coverage.outcomes()[1].state, culprit_core::SourceState::Collected);

    let mut io = FakeRead::new(vec![Ok(vec![row()]), Err(ReadError::Source(denied()))]);
    let mut coverage = Coverage::default();
    let context = application_with(&mut io, &mut coverage, "test grant").await;
    assert_eq!(context.tempdb_bytes, Some(0.0));
    assert!(context.persistent.is_none());
    assert!(context.persistent_signal().is_none());
    assert_eq!(context.tempdb_cleanup_context(), None, "an unreadable ADR setting is not disabled ADR");
    assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::Collected);
    assert_eq!(coverage.outcomes()[1].state, culprit_core::SourceState::Unavailable);
}

#[tokio::test]
async fn canceled_or_unconfirmed_store_reads_publish_no_fresh_raw_facts_or_size_signals() {
    for stop in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed] {
        for failure_at_first_read in [true, false] {
            let replies = if failure_at_first_read {
                vec![Err(ReadError::Stopped(stop))]
            } else {
                vec![Ok(vec![row()]), Err(ReadError::Stopped(stop))]
            };
            let mut io = FakeRead::new(replies);
            let mut coverage = Coverage::default();
            let context = application_with(&mut io, &mut coverage, "test grant").await;
            assert_eq!(context.tempdb_bytes, None);
            assert!(context.persistent.is_none());
            assert!(context.persistent_signal().is_none());
            assert_eq!(io.closed, 1);
            assert_eq!(io.calls.len(), if failure_at_first_read { 1 } else { 2 });
            assert!(coverage.outcomes().iter().all(|outcome| outcome.state == culprit_core::SourceState::Unavailable));
        }
    }
    let mut io = application_reader(Ok(vec![row()]));
    io.shutdown = SourceShutdown::Unconfirmed;
    let mut coverage = Coverage::default();
    let context = application_with(&mut io, &mut coverage, "test grant").await;
    assert!(context.persistent.is_none());
    assert_eq!(context.tempdb_bytes, None);
    assert_eq!(io.closed, 1);
    assert!(coverage.outcomes().iter().all(|outcome| outcome.state == culprit_core::SourceState::Unavailable));
}
