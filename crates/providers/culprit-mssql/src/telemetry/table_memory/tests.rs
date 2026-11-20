use super::*;
use crate::{
    read::{Parameter, ReadResult},
    resources::test_support::{FakeRead, replace},
    table_memory::tests as rows,
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
        selection: CollectionSelection::TableMemory { target: Box::new(rows::selection()) },
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
fn memory_source(
    data: Vec<ReadResult<Vec<Rec>>>,
    request: &CollectionRequest,
    generation: Option<u64>,
) -> (TableMemorySource<Reader>, Arc<AtomicU32>) {
    let closed = Arc::new(AtomicU32::new(0));
    let reader = Reader {
        io: FakeRead::new(data).with_query_limit(reader_budget(&request.budget).unwrap().max_queries),
        closed: closed.clone(),
        generation,
    };
    (TableMemorySource::from_reader(reader, request, "original.invalid".into(), 1433).unwrap(), closed)
}
async fn drain(source: &mut TableMemorySource<Reader>) -> Vec<TelemetryRecord> {
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!()
    };
    assert!(next_cursor.is_none());
    records
}
#[tokio::test]
async fn original_local_factory_profile_retains_exact_kibibytes_known_role_and_eleven_reader_dispatches() {
    for writable in [true, false] {
        let mut data = replies();
        if !writable {
            for index in [2, 3, 5, 7, 10] {
                let row = data[index].as_ref().unwrap()[0].clone();
                data[index] = Ok(vec![replace(&row, &[("updateability", Val::Text("READ_ONLY".into()))])]);
            }
        }
        let (mut source, closed) = memory_source(data, &request(), Some(1));
        let description = source.describe().await.unwrap();
        let records = drain(&mut source).await;
        let [TelemetryRecord::Operational(row)] = records.as_slice() else { panic!() };
        let OperationalFact::MssqlTableMemory(point) = &row.fact else { panic!() };
        assert!(point.fits_source(&description));
        assert_eq!(point.allocated_table_kibibytes.as_deref(), Some("9007199254740993"));
        assert_eq!(table_memory_context(&description).unwrap().database_writable, Some(writable));
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 11);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn missing_dmv_row_preserves_original_selected_header_without_synthetic_memory_zero() {
    let mut data = replies();
    data[5] = Ok(vec![replace(
        &rows::row(),
        &[
            ("row_present", Val::Int(0)),
            ("memory_allocated_for_table_kb", Val::Null),
            ("memory_used_by_table_kb", Val::Null),
            ("memory_allocated_for_indexes_kb", Val::Null),
            ("memory_used_by_indexes_kb", Val::Null),
        ],
    )]);
    let (mut source, _) = memory_source(data, &request(), Some(1));
    let description = source.describe().await.unwrap();
    assert!(drain(&mut source).await.is_empty());
    let SourceObservation::TableMemory { target, metadata, snapshot, .. } = &description.observation else { panic!() };
    assert_eq!(target.as_ref(), &rows::selection());
    assert!(metadata.available());
    assert_eq!(snapshot.sources[0].state, SourceState::Empty);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 11);
    source.close().await;
}
#[tokio::test]
async fn unavailable_native_feature_permission_layout_or_disk_table_preserves_scope_and_skips_dmv_work() {
    for (name, value, state) in [
        ("dmv_permission", Val::Int(0), SourceState::Unavailable),
        ("dmv_permission", Val::Null, SourceState::Unavailable),
        ("table_visibility", Val::Int(0), SourceState::Unavailable),
        ("xtp_supported", Val::Int(0), SourceState::Unavailable),
        ("layout_available", Val::Int(0), SourceState::Unavailable),
        ("is_memory_optimized", Val::Int(0), SourceState::NotApplicable),
    ] {
        let mut data = replies();
        for index in [3, 7] {
            let row = data[index].as_ref().unwrap()[0].clone();
            data[index] = Ok(vec![replace(&row, &[(name, value.clone())])]);
        }
        data.drain(4..6);
        let (mut source, _) = memory_source(data, &request(), Some(1));
        let description = source.describe().await.unwrap();
        assert!(drain(&mut source).await.is_empty());
        assert_eq!(description.observation.snapshot().unwrap().sources[0].state, state, "{name}");
        assert!(table_memory_context(&description).is_ok());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 9);
        source.close().await;
    }
}
#[tokio::test]
async fn native_before_a_frame_b_after_a_role_foreign_database_and_object_reuse_refuse() {
    for (name, value) in [
        ("updateability", Val::Text("READ_ONLY".into())),
        ("updateability", Val::Null),
        ("database_id", Val::Int(8)),
        ("object_id", Val::Int(101)),
        ("created_without_offset", Val::Text("2020-01-02T00:00:00.000".into())),
        ("modified_without_offset", Val::Text("2020-01-02T00:00:00.000".into())),
    ] {
        let mut data = replies();
        data[5] = Ok(vec![replace(&rows::row(), &[(name, value)])]);
        let (mut source, closed) = memory_source(data, &request(), Some(1));
        assert!(source.describe().await.is_err(), "{name}");
        assert_eq!(source.query_count(), 6);
        source.close().await;
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn final_native_member_process_database_object_or_role_change_withdraws_original_completion() {
    for change in ["object", "member", "process", "database", "role", "permission"] {
        let mut data = replies();
        let index = match change {
            "object" | "permission" => 7,
            "member" => 8,
            "process" => 9,
            _ => 10,
        };
        let row = data[index].as_ref().unwrap()[0].clone();
        let (name, value) = match change {
            "object" => ("modified_without_offset", Val::Text("2020-01-02T00:00:00.000".into())),
            "permission" => ("dmv_permission", Val::Int(0)),
            "member" => ("process_id", Val::Int(43)),
            "process" => ("started_at_ticks", Val::Int(101)),
            "database" => ("database_guid", Val::Text("recreated".into())),
            _ => ("updateability", Val::Text("READ_ONLY".into())),
        };
        data[index] = Ok(vec![replace(&row, &[(name, value)])]);
        let (mut source, _) = memory_source(data, &request(), Some(1));
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)), "{change}");
        source.close().await;
    }
}
#[tokio::test]
async fn bounded_caller_setup_reservation_cancel_and_deadline_failures_never_start_later_work() {
    assert_eq!(bounded_budget(&CollectionBudget::upper_limits()).max_queries, 12);
    assert_eq!(reader_budget(&CollectionBudget::upper_limits()).unwrap().max_queries, 11);
    let mut one = request().budget;
    one.max_queries = 1;
    assert!(reader_budget(&one).is_err());
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
    ] {
        let mut data = replies();
        data[5] = Err(ReadError::Stopped(stop));
        let (mut source, closed) = memory_source(data, &request(), Some(1));
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), stop);
        assert_eq!(source.query_count(), 6);
        source.close().await;
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
    let mut request = request();
    request.budget.max_queries = 6;
    let (mut source, _) = memory_source(replies(), &request, Some(1));
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 5);
    source.close().await;
}
#[tokio::test]
async fn unknown_original_connection_and_oversized_encoded_capture_never_look_empty() {
    let (mut source, _) = memory_source(replies(), &request(), None);
    assert!(source.describe().await.is_err());
    assert_eq!(source.query_count(), 3);
    source.close().await;
    let mut request = request();
    request.budget.page_bytes = 1024;
    request.budget.total_bytes = 1024;
    let (mut source, _) = memory_source(replies(), &request, Some(1));
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::ByteLimit);
    source.close().await;
}

#[tokio::test]
async fn hidden_native_target_and_unsupported_platform_do_not_publish_a_zero_memory_header() {
    let mut data = replies();
    data[3] = Ok(vec![]);
    let (mut source, closed) = memory_source(data, &request(), Some(1));
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceUnavailable);
    assert_eq!(source.query_count(), 4);
    source.close().await;
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    let mut data = replies();
    let original = data[0].as_ref().unwrap()[0].clone();
    data[0] = Ok(vec![replace(&original, &[("major", Val::Text("15".into()))])]);
    let original = data[2].as_ref().unwrap()[0].clone();
    data[2] = Ok(vec![replace(&original, &[("major", Val::Int(15))])]);
    let (mut source, _) = memory_source(data, &request(), Some(1));
    assert_eq!(source.describe().await.unwrap_err().code, ErrorCode::Unsupported);
    assert_eq!(source.query_count(), 3);
    source.close().await;
}
