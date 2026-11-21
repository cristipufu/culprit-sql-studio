use super::*;
use crate::{
    read::{NativeStreamRead, Parameter, ReadResult},
    resources::{
        managed_instance::tests::{StreamRead, empty_frame, first, frame, layout_rows, metadata},
        test_support::replace,
    },
    value::{Rec, Val},
};
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Default)]
struct Calls {
    queries: AtomicU32,
    closed: AtomicU32,
}
struct ObservedRead {
    native: StreamRead,
    calls: Arc<Calls>,
    changed_at: Option<u32>,
    unknown: bool,
}
impl ObservedRead {
    fn new(replies: Vec<ReadResult<Vec<Rec>>>) -> Self {
        Self {
            native: StreamRead::new(replies).with_query_limit(11),
            calls: Arc::new(Calls::default()),
            changed_at: None,
            unknown: false,
        }
    }
}
#[async_trait]
impl NativeRead for ObservedRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.calls.queries.fetch_add(1, Ordering::SeqCst);
        self.native.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.native.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        if self.unknown {
            None
        } else {
            Some(if self.changed_at.is_some_and(|at| self.queries() >= at) { 2 } else { 1 })
        }
    }
    async fn close(&mut self) -> SourceShutdown {
        self.calls.closed.fetch_add(1, Ordering::SeqCst);
        self.native.close().await
    }
}
#[async_trait]
impl NativeStreamRead for ObservedRead {
    async fn begin_stream(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<()> {
        self.calls.queries.fetch_add(1, Ordering::SeqCst);
        self.native.begin_stream(sql, parameters, limit).await
    }
    async fn next_batch(&mut self) -> ReadResult<crate::read::ReadBatch> {
        self.native.next_batch().await
    }
}

fn request() -> CollectionRequest {
    let owner = instance::observation(&metadata("2020-01-01T06:09:59Z")).unwrap().identity().unwrap().clone();
    CollectionRequest {
        subject: CollectionSubject::Shared { resource: owner },
        selection: CollectionSelection::ResourceHistory {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T06:01:00Z".into(),
        },
        budget: CollectionBudget::default(),
    }
}
fn replies(rows: Vec<Rec>) -> Vec<ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![metadata("2020-01-01T06:09:59Z")]),
        Ok(layout_rows()),
        Ok(vec![crate::instance::test_support::epoch(100)]),
        Ok(rows),
        Ok(vec![metadata("2020-01-01T06:10:01Z")]),
        Ok(vec![crate::instance::test_support::epoch(100)]),
    ]
}
fn history(description: &CollectionDescription) -> &MssqlManagedInstanceResourceHistory {
    let SourceObservation::MssqlManagedInstanceResources(history) = &description.observation else { panic!() };
    history
}
async fn drain(source: &mut ManagedInstanceResourceSource<ObservedRead>) -> Vec<TelemetryRecord> {
    let mut cursor = None;
    let mut all = Vec::new();
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::ResourceSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        all.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            return all;
        }
    }
}
#[tokio::test]
async fn frozen_pages_keep_the_original_history_and_one_reader_until_exact_final_verification() {
    let mut request = request();
    request.budget.page_rows = 1;
    let reader = ObservedRead::new(replies(vec![first(), frame("2020-01-01T06:00:15Z", "2020-01-01T06:00:30Z")]));
    let calls = reader.calls.clone();
    let mut source = ManagedInstanceResourceSource::with_io(reader, &request).unwrap();
    assert_eq!(source.query_count(), 0);
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request));
    assert_eq!(description.partition, SourcePartition::Target);
    assert_eq!(description.continuity, SourceContinuity::Unverified);
    assert_eq!(history(&description).coverage.records, 2);
    assert_eq!(history(&description).observed_at, "2020-01-01T06:10:00.000000000Z");
    assert_eq!(source.query_count(), 4);
    let records = drain(&mut source).await;
    assert_eq!(records.len(), 2);
    assert_eq!(source.query_count(), 4, "frozen paging sends no more history queries");
    let capture = CaptureIdentity { observation_id: "mi-native-source".into(), revision: None };
    for record in records {
        ExportedRecord::new(&description, &capture, record).unwrap();
    }
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(calls.closed.load(Ordering::SeqCst), 1);
    assert!(source.describe().await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 6);
}
#[tokio::test]
async fn native_lookahead_yields_a_retained_partial_prefix_without_extra_population_reads() {
    let mut request = request();
    request.budget.max_snapshot_rows = 1;
    let reader = ObservedRead::new(replies(vec![first(), frame("2020-01-01T06:00:15Z", "2020-01-01T06:00:30Z")]));
    let mut source = ManagedInstanceResourceSource::with_io(reader, &request).unwrap();
    let description = source.describe().await.unwrap();
    let history = history(&description);
    assert_eq!(history.coverage.state, culprit_core::SourceState::Partial);
    assert!(!history.coverage.complete_scope);
    assert_eq!(history.coverage.stopped, Some(CollectionStop::RowLimit));
    assert_eq!(history.native_rows, 2);
    assert_eq!(drain(&mut source).await.len(), 1);
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 6);
    source.close().await;
}
#[tokio::test]
async fn delayed_empty_and_native_permission_or_layout_gaps_are_not_a_healthy_zero() {
    let mut source =
        ManagedInstanceResourceSource::with_io(ObservedRead::new(replies(vec![empty_frame()])), &request()).unwrap();
    let description = source.describe().await.unwrap();
    assert_eq!(history(&description).coverage.state, culprit_core::SourceState::Partial);
    assert_eq!(history(&description).coverage.records, 0);
    assert!(!history(&description).coverage.complete_scope);
    assert!(drain(&mut source).await.is_empty());
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    source.close().await;
    for denied in [false, true] {
        let layout = if denied {
            layout_rows().iter().map(|row| replace(row, &[("server_state", Val::Int(0))])).collect()
        } else {
            layout_rows()
                .into_iter()
                .filter(|row| crate::read::text(row, "native_column").unwrap() != "start_time")
                .collect()
        };
        let responses =
            vec![Ok(vec![metadata("2020-01-01T06:09:59Z")]), Ok(layout), Ok(vec![metadata("2020-01-01T06:10:01Z")])];
        let mut source = ManagedInstanceResourceSource::with_io(ObservedRead::new(responses), &request()).unwrap();
        let description = source.describe().await.unwrap();
        assert_eq!(history(&description).coverage.state, culprit_core::SourceState::Unavailable);
        assert_eq!(
            history(&description).coverage.error_code,
            Some(if denied { ErrorCode::PermissionDenied } else { ErrorCode::Unsupported })
        );
        assert!(history(&description).current_reader.is_none());
        assert!(drain(&mut source).await.is_empty());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 3, "no denied/unsupported history or epoch query");
        source.close().await;
    }
}
#[tokio::test]
async fn boxed_instances_do_not_dispatch_managed_instance_layout_or_history_sql() {
    let before = replace(&metadata("2020-01-01T06:09:59Z"), &[("edition", Val::Int(3))]);
    let after = replace(&metadata("2020-01-01T06:10:01Z"), &[("edition", Val::Int(3))]);
    let mut source =
        ManagedInstanceResourceSource::with_io(ObservedRead::new(vec![Ok(vec![before]), Ok(vec![after])]), &request())
            .unwrap();
    let description = source.describe().await.unwrap();
    assert_eq!(history(&description).coverage.error_code, Some(ErrorCode::Unsupported));
    assert_eq!(history(&description).native_engine_edition, 3);
    assert!(drain(&mut source).await.is_empty());
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 2);
    source.close().await;
}
#[tokio::test]
async fn final_member_process_epoch_clock_and_connection_changes_withdraw_the_original_capture() {
    for label in ["member", "process", "epoch", "clock", "connection"] {
        let mut responses = replies(vec![first()]);
        match label {
            "member" => {
                responses[4] =
                    Ok(vec![replace(&metadata("2020-01-01T06:10:01Z"), &[("server_name", Val::Text("other".into()))])])
            }
            "process" => {
                responses[4] = Ok(vec![replace(&metadata("2020-01-01T06:10:01Z"), &[("process_id", Val::Int(43))])])
            }
            "epoch" => responses[5] = Ok(vec![crate::instance::test_support::epoch(101)]),
            "clock" => responses[4] = Ok(vec![metadata("2020-01-01T06:09:59Z")]),
            _ => {}
        }
        let mut reader = ObservedRead::new(responses);
        if label == "connection" {
            reader.changed_at = Some(5)
        }
        let mut source = ManagedInstanceResourceSource::with_io(reader, &request()).unwrap();
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed, "{label}");
        source.close().await;
    }
}
#[tokio::test]
async fn unknown_or_changed_original_connection_and_cancelled_description_cannot_authorize_followup_sql() {
    for label in ["unknown", "changed-layout", "changed-epoch", "changed"] {
        let mut reader = ObservedRead::new(replies(vec![first()]));
        reader.unknown = label == "unknown";
        reader.changed_at = match label {
            "changed-layout" => Some(2),
            "changed-epoch" => Some(3),
            "changed" => Some(4),
            _ => None,
        };
        let mut source = ManagedInstanceResourceSource::with_io(reader, &request()).unwrap();
        assert!(source.describe().await.is_err(), "{label}");
        let queries = source.query_count();
        assert_eq!(
            queries,
            match label {
                "unknown" => 1,
                "changed-layout" => 2,
                "changed-epoch" => 3,
                _ => 4,
            },
            "{label}"
        );
        assert!(source.describe().await.is_err());
        assert!(source.finish().await.is_err());
        assert_eq!(source.query_count(), queries);
        source.close().await;
    }
    let mut reader = ObservedRead::new(vec![]);
    reader.native = StreamRead::new(vec![]).pause_on_query(1);
    reader.native.buffered.shutdown = SourceShutdown::Unconfirmed;
    let calls = reader.calls.clone();
    let mut source = ManagedInstanceResourceSource::with_io(reader, &request()).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(1), source.describe()).await.is_err());
    assert!(source.describe().await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 1);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(calls.closed.load(Ordering::SeqCst), 1);
}
#[test]
fn bounded_window_and_native_frame_reserve_lookahead_and_keep_the_total_including_setup() {
    let budget = bounded_budget(&CollectionBudget::upper_limits());
    assert_eq!(budget.max_queries, 12);
    assert_eq!(crate::read::ReadSession::budget_after_setup(budget.clone()).unwrap().max_queries, 11);
    assert_eq!(budget.statement_timeout_ms, 1_000);
    assert_eq!(budget.total_timeout_ms, 5_000);
    assert_eq!(native_limit(&budget), 512);
    let mut tiny = budget.clone();
    tiny.page_bytes = 1024;
    assert_eq!(native_limit(&tiny), 512, "native total_bytes and delivery page_bytes have separate owners");
    tiny.max_pages = 1;
    assert_eq!(native_limit(&tiny), 64, "retain the caller's explicit total delivery-row ceiling");
    let mut request = request();
    request.selection = CollectionSelection::ResourceHistory {
        from: "2020-01-01T06:00:00Z".into(),
        until: "2020-01-01T09:00:00Z".into(),
    };
    assert!(ManagedInstanceResourceSource::with_io(ObservedRead::new(vec![]), &request).is_err());
}

#[tokio::test]
async fn two_hour_native_history_spans_delivery_pages_with_one_paid_native_query() {
    let start = chrono::DateTime::parse_from_rfc3339("2020-01-01T06:00:00Z").unwrap();
    let rows = (0..480)
        .map(|index| {
            let end = start + chrono::Duration::seconds(index * 15);
            replace(
                &frame(&(end - chrono::Duration::seconds(15)).to_rfc3339(), &end.to_rfc3339()),
                &[("observed_at", Val::Text("2020-01-01T08:10:00Z".into()))],
            )
        })
        .collect();
    let mut responses = replies(rows);
    responses[0] = Ok(vec![metadata("2020-01-01T08:09:59Z")]);
    responses[4] = Ok(vec![metadata("2020-01-01T08:10:01Z")]);
    let mut request = request();
    request.selection = CollectionSelection::ResourceHistory {
        from: "2020-01-01T06:00:00Z".into(),
        until: "2020-01-01T08:00:00Z".into(),
    };
    let mut source = ManagedInstanceResourceSource::with_io(ObservedRead::new(responses), &request).unwrap();
    let description = source.describe().await.unwrap();
    assert_eq!(history(&description).coverage.records, 480);
    assert_eq!(history(&description).native_rows, 480);
    assert_eq!(source.query_count(), 4);
    let records = drain(&mut source).await;
    assert_eq!(records.len(), 480);
    assert_eq!(source.query_count(), 4, "all native batches belong to one selected query, not repeated reads");
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn cancellation_with_a_started_stream_retires_the_same_owner_before_any_final_query() {
    let mut reader = ObservedRead::new(replies(vec![first()]));
    reader.native.pause_batches = true;
    reader.native.buffered.shutdown = SourceShutdown::Unconfirmed;
    let calls = reader.calls.clone();
    let mut source = ManagedInstanceResourceSource::with_io(reader, &request()).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(1), source.describe()).await.is_err());
    assert_eq!(source.query_count(), 4);
    assert!(source.describe().await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(calls.closed.load(Ordering::SeqCst), 1);
}
