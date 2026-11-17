use super::*;
use crate::read::{ReadError, ReadResult};

fn description(availability: i64) -> Vec<ReadResult<Vec<Rec>>> {
    let mut replies = description_rows();
    let Ok(options) = &mut replies[2] else { unreachable!() };
    options[0] = replace(&options[0], &[("page_server_read_availability", Val::Int(availability))]);
    replies
}

fn runtime(availability: i64) -> Rec {
    let mut changes = vec![("page_server_read_availability", Val::Int(availability))];
    if availability == 1 {
        changes.extend([
            ("avg_page_server_io_reads", Val::Float(12.5)),
            ("last_page_server_io_reads", Val::Int(9_007_199_254_740_993)),
            ("min_page_server_io_reads", Val::Int(0)),
            ("max_page_server_io_reads", Val::Int(i64::MAX)),
            ("stdev_page_server_io_reads", Val::Float(1.25)),
        ]);
    }
    replace(&identified_runtime(1, 3_000), &changes)
}

#[tokio::test]
async fn same_native_page_retains_all_statistics_and_distinct_original_availability_without_more_queries() {
    for (code, availability) in [
        (0, PageServerReadAvailability::NotApplicable),
        (1, PageServerReadAvailability::Available),
        (2, PageServerReadAvailability::Unknown),
        (3, PageServerReadAvailability::Unavailable),
    ] {
        let mut replies = description(code);
        replies[6] = Ok(vec![Rec::of(&[("plan_id", Val::Int(4))])]);
        replies.extend([Ok(vec![pressure()]), Ok(vec![runtime(code)])]);
        let mut source = Source::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
        let original = source.describe().await.unwrap();
        assert_eq!(
            original.query_history().unwrap().native_properties[PageServerReadAvailability::PROPERTY],
            availability.native_property()
        );
        let CollectionRead::Page { records, next_cursor: None } =
            source.read_page(TelemetryStream::QueryRuntime, None).await.unwrap()
        else {
            panic!("one native runtime page")
        };
        let TelemetryRecord::QueryRuntime(point) = &records[0] else { panic!("runtime") };
        let statistics = point.page_server_reads.as_ref().unwrap();
        assert_eq!(statistics.availability, availability);
        assert!(point.page_server_reads_match_source(&original));
        if code == 1 {
            assert_eq!(statistics.last_pages.as_deref(), Some("9007199254740993"));
            assert_eq!(statistics.minimum_pages.as_deref(), Some("0"));
            assert_eq!(statistics.standard_deviation_pages, Some(1.25));
        } else {
            assert!(!statistics.has_values());
        }
        assert_eq!(point.key.native.qualifiers["replica_group_id"], "1");
        assert_eq!(point.key.native.qualifiers["execution_type"], "3");
        assert_eq!(source.query_count(), 9, "metadata/pressure/page reads are unchanged");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn profile_change_during_page_read_is_source_changed_and_cannot_retry_under_old_header() {
    let mut replies = description(1);
    replies[6] = Ok(vec![Rec::of(&[("plan_id", Val::Int(4))])]);
    replies.extend([Ok(vec![pressure()]), Ok(vec![runtime(2)])]);
    let mut source = Source::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
    source.describe().await.unwrap();
    assert!(matches!(
        source.read_page(TelemetryStream::QueryRuntime, None).await.unwrap(),
        CollectionRead::Stopped(CollectionStop::SourceChanged)
    ));
    let acquired = source.query_count();
    assert!(source.read_page(TelemetryStream::QueryRuntime, None).await.is_err());
    assert_eq!(source.query_count(), acquired);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn original_capability_is_rechecked_even_for_empty_native_rows_without_claiming_healthy_zero() {
    let mut replies = description(1);
    replies.extend([
        Ok(vec![pressure()]),
        Ok(vec![]),
        Ok(vec![pressure()]),
        Ok(vec![]),
        Ok(vec![pressure()]),
        Ok(vec![pressure()]),
        Ok(vec![pressure()]),
    ]);
    replies.extend(description(3).into_iter().skip(2));
    let mut source = Source::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
    source.describe().await.unwrap();
    for stream in [
        TelemetryStream::QueryRuntime,
        TelemetryStream::QueryWaits,
        TelemetryStream::QueryDefinitions,
        TelemetryStream::PlanDefinitions,
    ] {
        assert!(
            matches!(source.read_page(stream, None).await.unwrap(), CollectionRead::Page { records, next_cursor: None } if records.is_empty())
        );
    }
    assert!(!source.finish().await.unwrap().is_verified());
    assert_eq!(source.query_count(), 19, "the existing final description owns this check");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn stopped_runtime_projection_never_falls_back_or_loses_unconfirmed_shutdown() {
    for stop in [
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
        CollectionStop::TimedOut,
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
    ] {
        let mut replies = description(1);
        replies.extend([Ok(vec![pressure()]), Err(ReadError::Stopped(stop))]);
        let mut io = FakeRead::new(replies);
        if stop == CollectionStop::CancellationUnconfirmed {
            io.shutdown = SourceShutdown::Unconfirmed;
        }
        let mut source = Source::with_io(io, &request(), "fixture.invalid".into(), 1433).unwrap();
        source.describe().await.unwrap();
        assert!(
            matches!(source.read_page(TelemetryStream::QueryRuntime, None).await.unwrap(), CollectionRead::Stopped(actual) if actual==stop)
        );
        let acquired = source.query_count();
        assert!(source.read_page(TelemetryStream::QueryRuntime, None).await.is_err());
        assert_eq!(source.query_count(), acquired);
        assert_eq!(
            source.close().await,
            if stop == CollectionStop::CancellationUnconfirmed {
                SourceShutdown::Unconfirmed
            } else {
                SourceShutdown::Quiescent
            }
        );
    }
}
