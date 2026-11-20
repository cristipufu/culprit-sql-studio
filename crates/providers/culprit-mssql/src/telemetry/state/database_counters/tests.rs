use super::super::*;
use crate::{
    instance::test_support::{epoch, metadata},
    performance::test_support::{Reader, authority, frames},
    read::ReadResult,
    resources::test_support::{instance_pressure_row, replace},
    value::{Rec, Val},
};
use culprit_core::SourceState;
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::DatabasePerformanceCounters },
        budget: CollectionBudget::default(),
    }
}
fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(3))])]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:00Z")]),
        Ok(vec![super::super::tests::clock(true, "2020-01-01T06:00:00Z")]),
        Ok(vec![instance_pressure_row()]),
        Ok(frames()),
        Ok(vec![super::super::tests::clock(true, "2020-01-01T06:00:01Z")]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:02Z")]),
    ]
}
fn source(rows: Vec<ReadResult<Vec<Rec>>>, request: &CollectionRequest) -> StateSource<Reader> {
    let mut reader = Reader::new(rows);
    reader.inner = reader.inner.with_query_limit(local_point_budget(&request.budget).max_queries);
    StateSource::with_io(reader, request, "fixture.invalid".into(), 1433).unwrap()
}
async fn page(source: &mut StateSource<Reader>) -> Vec<TelemetryRecord> {
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("original counter page")
    };
    assert!(next_cursor.is_none());
    records
}
#[tokio::test]
async fn original_database_counter_state_uses_one_family_full_native_header_and_exact_final_owner() {
    let mut source = source(replies(), &request());
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request()));
    let context = database_performance_context(&description).unwrap();
    assert_eq!(context.database_writable, Some(true));
    assert!(context.target.database().unwrap().incarnation.is_none());
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.sources.len(), 1);
    assert_eq!(snapshot.sources[0].state, SourceState::Collected);
    let rows = page(&mut source).await;
    assert_eq!(rows.len(), 1);
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(sample), .. }) =
        &rows[0]
    else {
        panic!()
    };
    assert_eq!(sample.counters.len(), 3);
    assert!(database_performance_sample_fits_source(sample, &description));
    let SourcePartition::InstanceLocal { epoch: Some(epoch), .. } = &context.partition else { panic!() };
    assert_eq!(&sample.key().unwrap(), epoch);
    ExportedRecord::new(
        &description,
        &CaptureIdentity { observation_id: "original".into(), revision: None },
        rows[0].clone(),
    )
    .unwrap();
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 11);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.io.inner.closed, 1);
    assert_eq!(source.io.inner.calls.iter().filter(|call| call.sql.contains("AS catalog_matches")).count(), 1);
}
#[tokio::test]
async fn partial_or_denied_point_retains_original_database_header_without_fabricated_zeroes() {
    for denied in [false, true] {
        let mut rows = replies();
        rows[6] = if denied {
            Ok(vec![replace(&frames()[0], &[("catalog_visible", Val::Int(0))])])
        } else {
            Ok(frames().into_iter().take(2).collect())
        };
        let mut source = source(rows, &request());
        let description = source.describe().await.unwrap();
        let coverage = &description.observation.snapshot().unwrap().sources[0];
        assert_eq!(coverage.state, if denied { SourceState::Unavailable } else { SourceState::Partial });
        assert!(!coverage.complete_scope);
        assert!(database_performance_context(&description).is_ok());
        assert_eq!(page(&mut source).await.len(), usize::from(!denied));
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn middle_frame_database_or_role_cannot_borrow_matching_before_and_final_authority() {
    for (field, value) in [
        ("selected_database_id", Val::Int(8)),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("database_guid", Val::Text("other".into())),
    ] {
        let mut rows = replies();
        let mut frame = frames();
        frame[1] = replace(&frame[1], &[(field, value)]);
        rows[6] = Ok(frame);
        let mut source = source(rows, &request());
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
        assert_eq!(source.query_count(), 7);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn final_database_process_role_or_socket_change_refuses_original_completion() {
    for change in ["database", "process", "role", "connection", "clock"] {
        let mut rows = replies();
        match change {
            "database" => {
                rows[10] = Ok(vec![replace(
                    &authority("2020-01-01T06:00:02Z"),
                    &[("database_guid", Val::Text("other".into()))],
                )])
            }
            "process" => rows[9] = Ok(vec![epoch(101)]),
            "role" => {
                rows[10] = Ok(vec![replace(
                    &authority("2020-01-01T06:00:02Z"),
                    &[("updateability", Val::Text("READ_ONLY".into()))],
                )])
            }
            "clock" => rows[10] = Ok(vec![authority("2019-01-01T00:00:00Z")]),
            _ => {}
        }
        let mut source = source(rows, &request());
        if change == "connection" {
            source.io.change_at = Some(9);
        }
        source.describe().await.unwrap();
        page(&mut source).await;
        assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)), "{change}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn cancellation_timeout_or_pressure_never_dispatches_or_finishes_a_new_original_point() {
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::TimedOut,
        CollectionStop::Pressure,
    ] {
        let mut rows = replies();
        rows[6] = Err(ReadError::Stopped(stop));
        let mut source = source(rows, &request());
        source.io.inner.shutdown = if stop == CollectionStop::CancellationUnconfirmed {
            SourceShutdown::Unconfirmed
        } else {
            SourceShutdown::Quiescent
        };
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), stop);
        assert_eq!(source.query_count(), 7);
        assert!(source.finish().await.is_err());
        let expected = source.io.inner.shutdown;
        assert_eq!(source.close().await, expected);
        assert_eq!(source.io.inner.closed, 1);
    }
}
#[tokio::test]
async fn unsupported_platform_unknown_or_readonly_owner_refuses_before_counter_dispatch() {
    for (field, value) in
        [("updateability", Val::Null), ("updateability", Val::Text("READ_ONLY".into())), ("database_guid", Val::Null)]
    {
        let mut rows = replies();
        rows[3] = Ok(vec![replace(&authority("2020-01-01T06:00:00Z"), &[(field, value)])]);
        let mut source = source(rows, &request());
        assert!(source.describe().await.is_err());
        assert_eq!(source.query_count(), 4);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    for (edition, major) in [(3, 15), (8, 16)] {
        let mut rows = replies();
        rows[0] = Ok(vec![Rec::of(&[("edition", Val::Int(edition))])]);
        rows[1] =
            Ok(vec![replace(&metadata(), &[("edition", Val::Int(edition)), ("major", Val::Text(major.to_string()))])]);
        rows[3] = Ok(vec![replace(
            &authority("2020-01-01T06:00:00Z"),
            &[("edition", Val::Int(edition)), ("major", Val::Int(major))],
        )]);
        let mut source = source(rows, &request());
        assert!(source.describe().await.is_err());
        assert_eq!(source.query_count(), 4);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[test]
fn independent_caller_limits_are_intersected_without_automatic_cost_or_default_admission() {
    let upper = super::super::local_point::bounded_budget(&CollectionBudget::upper_limits());
    assert_eq!(upper.max_queries, 12);
    assert_eq!(upper.max_snapshot_rows, 1);
    assert_eq!(upper.page_rows, 1);
    assert_eq!(upper.statement_timeout_ms, 1000);
    assert_eq!(upper.total_timeout_ms, 5000);
    let lower = CollectionBudget {
        max_queries: 4,
        page_bytes: 1024,
        total_bytes: 2048,
        statement_timeout_ms: 20,
        total_timeout_ms: 40,
        ..upper
    };
    assert_eq!(super::super::local_point::bounded_budget(&lower), lower);
    assert!(point::shared_families(SnapshotDataset::DatabasePerformanceCounters).is_err());
}
#[tokio::test]
async fn a_small_query_cap_cannot_skip_final_native_database_verification() {
    let mut request = request();
    request.budget.max_queries = 10;
    let mut source = source(replies(), &request);
    source.describe().await.unwrap();
    page(&mut source).await;
    assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 10);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
