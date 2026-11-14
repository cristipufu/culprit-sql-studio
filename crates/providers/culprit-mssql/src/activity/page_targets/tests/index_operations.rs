use super::*;
use crate::index_operations::{targeted::tests::point_row, tests::layout_row};

const LAYOUT: usize = 6;
const SECOND_ADMISSION: usize = 7;
const POINT: usize = 8;
const LAST_HEADER: usize = 9;
const LAST_REQUEST: usize = 10;

fn checked_header(ordinal: usize, page: i64) -> Rec {
    replace(&header(ordinal, page), &[("catalog_partition_number", Val::Int(1))])
}

pub(crate) fn replies(rows: Vec<Rec>) -> Vec<ReadResult<Vec<Rec>>> {
    let mut replies = successful_replies(rows);
    let headers = match &replies[HEADER_BEFORE] {
        Ok(headers) => {
            headers.iter().map(|row| replace(row, &[("catalog_partition_number", Val::Int(1))])).collect::<Vec<_>>()
        }
        Err(_) => panic!("successful header fixture"),
    };
    replies[HEADER_BEFORE] = Ok(headers.clone());
    replies[HEADER_AFTER] = Ok(headers);
    replies.insert(LAYOUT, Ok(vec![layout_row(true, &[])]));
    replies.insert(POINT, Ok(vec![point_row()]));
    replies
}

pub(crate) fn paused_point_reader() -> ActivityReader<FakeRead> {
    let mut native = reader(replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]));
    native.io = native.io.pause_on_query(POINT + 1);
    native.io.shutdown = SourceShutdown::Unconfirmed;
    native
}

fn point(row: &PageWaitObservation) -> &PageIndexOperations {
    row.index_operations.as_ref().expect("native index point")
}

#[tokio::test]
async fn same_index_participants_and_pages_share_one_exact_counter_read_and_keep_independent_totals() {
    let rows = vec![
        page_wait(42, "PAGELATCH_EX", "7:1:4144"),
        page_wait(43, "PAGEIOLATCH_SH", "7:1:4144"),
        page_wait(44, "PAGELATCH_EX", "7:1:4145"),
    ];
    let mut native = reader(replies(rows));
    let captured = native.capture_page_wait_targets().await.unwrap();
    assert_eq!(native.query_count(), 12);
    assert!(captured.snapshot.is_valid());
    for row in &captured.snapshot.observations {
        let PageIndexOperations::Observed { observed_at, counters, partition_id, relation, .. } = point(row) else {
            panic!()
        };
        assert_eq!(*relation, PageIndexOperationRelation::CheckedTarget);
        assert_eq!(counters.counter(MssqlIndexCounter::PageLatchWaitMs), Some("9007199254740993"));
        assert_eq!(partition_id.as_deref(), Some("9007199254740993"));
        assert_eq!(counters.reset, culprit_core::ResetMarker::Unknown);
        assert_eq!(observed_at, "2020-01-01T06:00:00.000000000Z");
        assert_eq!(row.request.wait.as_ref().unwrap().elapsed_ms.as_deref(), Some("1"));
    }
    let sql = &native.io.calls[POINT].sql;
    assert_eq!(sql.matches("sys.dm_db_index_operational_stats(").count(), 1);
    assert_eq!(native.io.calls[POINT].parameters.len(), 5);
    assert_eq!(native.io.calls[POINT].limit, 2);
    assert!(sql.contains("(@P1,@P2,@P3,@P4)"));
    assert!(!sql.contains("NULL,NULL") && !sql.contains("ORDER BY") && !sql.contains("GROUP BY"));
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn ended_or_replaced_requests_do_not_erase_independently_mapped_index_points() {
    let initial = page_wait(42, "PAGELATCH_EX", "7:1:4144");
    for (column, value) in [("checked_wait_type", Val::Null), ("checked_begin", Val::Int(2))] {
        let mut supplied = replies(vec![initial.clone()]);
        supplied[LAST_REQUEST] = Ok(vec![replace(&participant(0, &initial), &[(column, value)])]);
        let captured = reader(supplied).capture_page_wait_targets().await.unwrap();
        let row = &captured.snapshot.observations[0];
        assert!(matches!(
            point(row),
            PageIndexOperations::Observed { relation: PageIndexOperationRelation::CheckedTarget, .. }
        ));
        assert_eq!(row.request.wait.as_ref().unwrap().elapsed_ms.as_deref(), Some("1"));
        if column == "checked_begin" {
            assert!(row.statement_identity.is_none());
        }
    }
}

#[tokio::test]
async fn acquired_points_survive_changed_headers_or_lost_catalog_readability_without_a_false_page_link() {
    for changed in [true, false] {
        let mut supplied = replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
        if changed {
            supplied[LAST_HEADER] = Ok(vec![replace(&checked_header(0, 4144), &[("partition_id", Val::Int(123))])]);
        } else {
            supplied[LAST_HEADER] =
                Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "fixture")));
            let removed = supplied.remove(LAST_REQUEST);
            assert!(removed.is_ok());
        }
        let captured = reader(supplied).capture_page_wait_targets().await.unwrap();
        let row = &captured.snapshot.observations[0];
        assert!(row.target.is_none());
        let PageIndexOperations::Observed { counters, relation, .. } = point(row) else { panic!() };
        assert_eq!(
            *relation,
            if changed {
                PageIndexOperationRelation::ChangedTarget
            } else {
                PageIndexOperationRelation::UnverifiedTarget
            }
        );
        assert_eq!(counters.counter(MssqlIndexCounter::PageLatchWaits), Some("3"));
        assert!(captured.snapshot.is_valid());
    }
}

#[tokio::test]
async fn missing_denied_and_unavailable_counter_reads_preserve_headers_without_fabricating_zero() {
    for outcome in [
        Ok(vec![]),
        Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "fixture"))),
        Err(ReadError::Source(culprit_core::Error::new(ErrorCode::QueryFailed, "fixture"))),
    ] {
        let expected = match &outcome {
            Ok(_) => PageIndexOperations::Missing,
            Err(ReadError::Source(error)) if error.code == ErrorCode::PermissionDenied => {
                PageIndexOperations::PermissionDenied
            }
            _ => PageIndexOperations::Unavailable,
        };
        let mut supplied = replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
        supplied[POINT] = outcome;
        let captured = reader(supplied).capture_page_wait_targets().await.unwrap();
        let row = &captured.snapshot.observations[0];
        assert_eq!(row.resolution, PageWaitResolution::ObservedAtBothBounds);
        assert_eq!(point(row), &expected);
    }
}

#[tokio::test]
async fn unqualified_partition_numbers_or_allocation_pages_never_reach_the_counter_function() {
    for (column, value) in [
        ("catalog_partition_number", Val::Null),
        ("catalog_partition_number", Val::Int(0)),
        ("catalog_partition_number", Val::Int(i32::MAX as i64 + 1)),
        ("page_type", Val::Int(11)),
        ("index_type", Val::Int(5)),
    ] {
        let mut supplied = successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
        let header = replace(&checked_header(0, 4144), &[(column, value)]);
        supplied[HEADER_BEFORE] = Ok(vec![header.clone()]);
        supplied[HEADER_AFTER] = Ok(vec![header]);
        let mut native = reader(supplied);
        let captured = native.capture_page_wait_targets().await.unwrap();
        assert!(captured.snapshot.observations[0].index_operations.is_none());
        assert!(native.io.calls.iter().all(|call| !call.sql.contains("sys.dm_db_index_operational_stats")));
    }
}

#[tokio::test]
async fn pressure_cancellation_and_timeouts_stop_all_follow_up_sql_and_owned_close_still_runs() {
    let mut supplied = replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
    supplied[SECOND_ADMISSION] = Ok(vec![]);
    let mut native = reader(supplied);
    let error = native.capture_page_wait_targets().await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::PressureUnavailable);
    assert_eq!(native.query_count(), SECOND_ADMISSION as u32 + 1);
    assert!(native.io.calls.iter().all(|call| !call.sql.contains("sys.dm_db_index_operational_stats(")));
    assert_eq!(native.close().await, SourceShutdown::Quiescent);

    for phase in [LAYOUT, POINT] {
        for stop in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::QueryLimit] {
            let mut supplied = replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
            supplied[phase] = Err(ReadError::Stopped(stop));
            let mut native = reader(supplied);
            let (snapshot, coverage) = crate::page_waits::collect_from(&mut native).await;
            assert!(snapshot.is_none());
            assert_eq!(native.query_count(), phase as u32 + 1);
            assert_eq!(native.io.closed, 1);
            assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::Unavailable);
        }
    }
}

#[tokio::test]
async fn denied_column_visibility_does_not_read_counter_data_or_erase_the_independent_page_mapping() {
    let mut supplied = replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
    supplied[LAYOUT] = Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "fixture")));
    assert!(supplied.remove(POINT).is_ok());
    let mut native = reader(supplied);
    let captured = native.capture_page_wait_targets().await.unwrap();
    assert!(captured.snapshot.observations[0].target.is_some());
    assert_eq!(point(&captured.snapshot.observations[0]), &PageIndexOperations::PermissionDenied);
    assert!(native.io.calls.iter().all(|call| !call.sql.contains("sys.dm_db_index_operational_stats(")));
}

#[tokio::test]
async fn hidden_or_recreated_intermediate_catalog_keeps_a_positive_unlinked_point() {
    for (changes, expected) in [
        (vec![("object_created_without_offset", Val::Null)], PageIndexOperationRelation::UnverifiedTarget),
        (
            vec![("catalog_partition_id", Val::Null), ("catalog_hobt_id", Val::Null)],
            PageIndexOperationRelation::UnverifiedTarget,
        ),
        (
            vec![("object_modified_without_offset", Val::Text("2020-01-01T05:00:00".into()))],
            PageIndexOperationRelation::ChangedTarget,
        ),
    ] {
        let mut supplied = replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
        supplied[POINT] = Ok(vec![replace(&point_row(), &changes)]);
        let captured = reader(supplied).capture_page_wait_targets().await.unwrap();
        let row = &captured.snapshot.observations[0];
        assert!(row.target.is_some(), "independent header mapping remains visible");
        let PageIndexOperations::Observed { relation, counters, .. } = point(row) else { panic!() };
        assert_eq!(*relation, expected);
        assert_eq!(counters.counter(MssqlIndexCounter::PageLatchWaits), Some("3"));
        assert!(row.is_valid());
    }
}

#[tokio::test]
async fn regressed_native_point_clock_and_unconfirmed_close_cannot_publish_counters() {
    let mut supplied = replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
    supplied[POINT] =
        Ok(vec![replace(&point_row(), &[("observed_at", Val::Text("2020-01-01T06:00:00.000000001Z".into()))])]);
    let mut native = reader(supplied);
    let error = native.capture_page_wait_targets().await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert_eq!(native.close().await, SourceShutdown::Quiescent);

    let mut native = reader(replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]));
    native.io.shutdown = SourceShutdown::Unconfirmed;
    let (captured, coverage) = crate::page_waits::collect_from(&mut native).await;
    assert!(captured.is_none());
    assert_eq!(native.io.closed, 1);
    assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::Unavailable);
}
