use super::*;
use crate::{
    columnstore_rowgroups::tests as rows,
    read::{Parameter, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use std::sync::atomic::{AtomicU32, Ordering};
struct Reader {
    io: FakeRead,
    closed: Arc<AtomicU32>,
    generation: Option<u64>,
}
#[async_trait::async_trait]
impl NativeRead for Reader {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.io.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.io.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.generation
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed.fetch_add(1, Ordering::SeqCst);
        self.io.close().await
    }
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::ColumnstoreRowgroups { target: Box::new(rows::selection()) },
        budget: CollectionBudget::default(),
    }
}
fn authority(at: &str) -> Rec {
    replace(
        &crate::activity::database_context::tests::row(3),
        &[("selected_database_id", Val::Int(7)), ("observed_at", Val::Text(at.into()))],
    )
}
fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![crate::instance::test_support::metadata()]),
        Ok(vec![crate::instance::test_support::epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:00Z")]),
        Ok(vec![rows::target_row()]),
        Ok(vec![crate::resources::test_support::instance_pressure_row()]),
        Ok(vec![rows::row()]),
        Ok(vec![Rec::of(&[
            ("observed_at", Val::Text("2020-01-01T06:00:00.6Z".into())),
            ("metadata_visibility", Val::Int(1)),
        ])]),
        Ok(vec![replace(&rows::target_row(), &[("observed_at", Val::Text("2020-01-01T06:00:00.8Z".into()))])]),
        Ok(vec![crate::instance::test_support::metadata()]),
        Ok(vec![crate::instance::test_support::epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:01Z")]),
    ]
}
fn source(
    data: Vec<ReadResult<Vec<Rec>>>,
    request: &CollectionRequest,
    generation: Option<u64>,
) -> (RowgroupSource<Reader>, Arc<AtomicU32>) {
    source_with_query_limit(data, request, generation, request.budget.max_queries)
}
fn source_with_query_limit(
    data: Vec<ReadResult<Vec<Rec>>>,
    request: &CollectionRequest,
    generation: Option<u64>,
    query_limit: u32,
) -> (RowgroupSource<Reader>, Arc<AtomicU32>) {
    let closed = Arc::new(AtomicU32::new(0));
    let reader = Reader { io: FakeRead::new(data).with_query_limit(query_limit), closed: closed.clone(), generation };
    (RowgroupSource::from_reader(reader, request, "original.invalid".into(), 1433).unwrap(), closed)
}
async fn drain(source: &mut RowgroupSource<Reader>) -> Vec<TelemetryRecord> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        rows.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            return rows;
        }
    }
}

#[tokio::test]
async fn actual_selected_owner_known_updateability_and_eleven_dispatches_remain_original() {
    for writable in [true, false] {
        let mut data = replies();
        if !writable {
            for index in [2, 10] {
                let row = data[index].as_ref().unwrap()[0].clone();
                data[index] = Ok(vec![replace(&row, &[("updateability", Val::Text("READ_ONLY".into()))])]);
            }
        }
        let (mut source, closed) = source(data, &request(), Some(1));
        let description = source.describe().await.unwrap();
        let records = drain(&mut source).await;
        assert_eq!(records.len(), 1);
        let TelemetryRecord::Operational(row) = &records[0] else { panic!() };
        let OperationalFact::MssqlColumnstoreRowgroup(point) = &row.fact else { panic!() };
        assert!(point.fits_source(&description));
        assert_eq!(point.context.database_writable, Some(writable));
        assert_eq!(point.size_bytes.as_deref(), Some("9007199254740993"));
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 11);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn empty_catalog_and_one_row_limit_keep_qualified_selection_and_honest_native_coverage() {
    for empty in [true, false] {
        let mut data = replies();
        data[5] =
            Ok(if empty { vec![] } else { vec![rows::row(), replace(&rows::row(), &[("row_group_id", Val::Int(1))])] });
        let mut request = request();
        request.budget.max_snapshot_rows = 1;
        let (mut source, _) = source(data, &request, Some(1));
        let description = source.describe().await.unwrap();
        let records = drain(&mut source).await;
        let coverage = &description.observation.snapshot().unwrap().sources[0];
        assert_eq!(records.len(), usize::from(!empty));
        assert_eq!(coverage.complete_scope, empty);
        assert_eq!(coverage.stopped, (!empty).then_some(CollectionStop::RowLimit));
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 11);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn hidden_denied_unknown_or_changed_native_target_prevents_any_rowgroup_dispatch() {
    for change in ["hidden", "denied", "unknown", "changed", "layout", "memory"] {
        let mut data = replies();
        data[3] = Ok(match change {
            "hidden" => vec![],
            _ => {
                let (name, value) = match change {
                    "denied" => ("can_view_definition", Val::Int(0)),
                    "unknown" => ("can_view_definition", Val::Null),
                    "changed" => ("modified_without_offset", Val::Text("2020-01-02T00:00:00.000".into())),
                    "layout" => ("layout_available", Val::Int(0)),
                    _ => ("is_memory_optimized", Val::Int(1)),
                };
                vec![replace(&rows::target_row(), &[(name, value)])]
            }
        });
        let (mut source, closed) = source(data, &request(), Some(1));
        assert!(source.describe().await.is_err(), "{change}");
        assert_eq!(source.query_count(), 4);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn rowgroup_duplicates_late_clock_or_invalid_lookahead_cannot_publish_a_positive_prefix() {
    for change in ["duplicate", "future", "witness"] {
        let mut data = replies();
        data[5] = Ok(match change {
            "duplicate" => vec![rows::row(), rows::row()],
            "future" => vec![replace(&rows::row(), &[("observed_at", Val::Text("2020-01-01T06:00:02Z".into()))])],
            _ => vec![rows::row(), replace(&rows::row(), &[("row_group_id", Val::Int(1)), ("state", Val::Int(8))])],
        });
        let mut requested = request();
        requested.budget.max_snapshot_rows = 1;
        let (mut source, _) = source(data, &requested, Some(1));
        assert!(source.describe().await.is_err(), "{change}");
        assert_eq!(source.query_count(), 7);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn final_object_process_role_or_recreated_database_withdraws_original_completion() {
    for change in ["object", "member", "role", "database"] {
        let mut data = replies();
        let index = match change {
            "object" => 7,
            "member" => 8,
            _ => 10,
        };
        let row = data[index].as_ref().unwrap()[0].clone();
        let (name, value) = match change {
            "object" => ("modified_without_offset", Val::Text("2020-01-02T00:00:00.000".into())),
            "member" => ("process_id", Val::Int(43)),
            "role" => ("updateability", Val::Text("READ_ONLY".into())),
            _ => ("database_guid", Val::Text("recreated".into())),
        };
        data[index] = Ok(vec![replace(&row, &[(name, value)])]);
        let (mut source, _) = source(data, &request(), Some(1));
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)), "{change}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn caller_budget_and_native_cancel_fences_stop_before_additional_source_work() {
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
    ] {
        let mut data = replies();
        data[5] = Err(ReadError::Stopped(stop));
        let (mut source, closed) = source(data, &request(), Some(1));
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), stop);
        assert_eq!(source.query_count(), 6);
        source.close().await;
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
    let mut requested = request();
    requested.budget.max_queries = 5;
    let (mut source, _) = source(replies(), &requested, Some(1));
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 5);
    source.close().await;
}
#[tokio::test]
async fn unknown_original_connection_and_small_encoded_budget_do_not_look_empty() {
    let (mut missing, _) = source(replies(), &request(), None);
    assert!(missing.describe().await.is_err());
    assert_eq!(missing.query_count(), 3);
    missing.close().await;
    let mut requested = request();
    requested.budget.page_bytes = 1024;
    requested.budget.total_bytes = 1024;
    let (mut limited, _) = source(replies(), &requested, Some(1));
    assert_eq!(CollectionStop::from_error(&limited.describe().await.unwrap_err()), CollectionStop::ByteLimit);
    limited.close().await;
}

#[tokio::test]
async fn native_setup_slot_cannot_complete_final_ownership_but_full_original_profile_still_can() {
    for total in [6, 11, 12, 13] {
        let mut requested = request();
        requested.budget.max_queries = total;
        let caller = bounded_budget(&requested.budget);
        let reader = ReadSession::budget_after_setup(caller.clone()).unwrap();
        let (mut source, closed) = source_with_query_limit(replies(), &requested, Some(1), reader.max_queries);
        let described = source.describe().await;
        if total == 6 {
            assert_eq!(CollectionStop::from_error(&described.unwrap_err()), CollectionStop::QueryLimit);
        } else {
            described.unwrap();
            let captured = drain(&mut source).await;
            assert_eq!(captured.len(), 1);
            let completion = source.finish().await;
            if total >= 12 {
                assert_eq!(completion.unwrap(), SourceCompletion::Verified);
            } else {
                assert_eq!(CollectionStop::from_error(&completion.unwrap_err()), CollectionStop::QueryLimit);
            }
        }
        let count = source.query_count();
        assert_eq!(count + ReadSession::SESSION_SETUP_QUERIES, caller.max_queries);
        if total < 12 {
            assert!(!source.finish().await.is_ok_and(|completion| completion.is_verified()));
            assert_eq!(source.query_count(), count, "a refused final proof cannot renew the source allowance");
        }
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(closed.load(Ordering::SeqCst), 2, "each explicit close reaches the same reader");
        assert_eq!(source.query_count(), count, "repeated close starts no replacement query");
    }
}

#[tokio::test]
async fn cancellation_keeps_the_budgeted_original_reader_until_close_without_restarting() {
    let requested = request();
    let budget = ReadSession::budget_after_setup(bounded_budget(&requested.budget)).unwrap();
    let closed = Arc::new(AtomicU32::new(0));
    let reader = Reader {
        io: FakeRead::new(replies()).with_query_limit(budget.max_queries).pause_on_query(5),
        closed: closed.clone(),
        generation: Some(1),
    };
    let mut source = RowgroupSource::from_reader(reader, &requested, "original.invalid".into(), 1433).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(5), source.describe()).await.is_err());
    let count = source.query_count();
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), count);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
}
