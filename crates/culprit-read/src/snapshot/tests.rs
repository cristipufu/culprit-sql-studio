//! Every provider uses this lifecycle; native fixtures separately prove protocol completion and source semantics.
use super::*;
use culprit_core::{DatabaseIdentity, Engine, MeasurementScope, SourceState};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

const FROM: &str = "2026-11-01T00:00:00Z";
const UNTIL: &str = "2026-11-01T00:00:01Z";
const STREAM: TelemetryStream = TelemetryStream::OperationalSamples;

struct Row {
    value: u64,
    source: &'static str,
    complete_scope: bool,
}
fn row(value: u64) -> Row {
    Row { value, source: "fixture", complete_scope: true }
}
fn batch(values: &[u64], complete: bool) -> Result<ReadBatch<Row>> {
    Ok(ReadBatch { rows: values.iter().copied().map(row).collect(), complete })
}
struct Reader {
    calls: Arc<Mutex<Vec<&'static str>>>,
    batches: VecDeque<Result<ReadBatch<Row>>>,
    scopes: Vec<SnapshotScope>,
    changed: bool,
    block_begin: bool,
    shutdown: SourceShutdown,
}
impl Reader {
    fn new(batches: impl IntoIterator<Item = Result<ReadBatch<Row>>>) -> Self {
        Self {
            calls: Arc::default(),
            batches: batches.into_iter().collect(),
            scopes: vec![SnapshotScope::complete("fixture")],
            changed: false,
            block_begin: false,
            shutdown: SourceShutdown::Quiescent,
        }
    }
    fn called(&self, name: &'static str) {
        self.calls.lock().unwrap().push(name);
    }
}
#[async_trait::async_trait]
impl SnapshotReader for Reader {
    type Row = Row;
    async fn describe(&mut self, _: &CollectionRequest) -> Result<CollectionDescription> {
        self.called("describe");
        Ok(CollectionDescription {
            database: DatabaseIdentity::server(Engine::Postgres, "fixture", "fixture.invalid", 5432, None),
            source: "scripted-native-source".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PagedSnapshot(PagedSnapshotDescription {
                dataset: SnapshotDataset::Operational,
                observed_from: FROM.into(),
                sources: vec!["fixture".into()],
            }),
        })
    }
    async fn begin(&mut self, _: &CollectionRequest) -> Result<()> {
        self.called("begin");
        if self.block_begin {
            std::future::pending::<()>().await;
        }
        Ok(())
    }
    async fn next_batch(&mut self) -> Result<ReadBatch<Row>> {
        self.called("next");
        self.batches.pop_front().expect("the script must declare every native batch")
    }
    fn decode(&self, row: &Row, _: &CollectionDescription) -> Result<SnapshotRecord> {
        let counters = NativeCounterSet {
            identity: NativeIdentity::new(row.value.to_string()),
            scope: MeasurementScope::Database,
            counters: vec![NativeCounterSample::gauge(
                "count",
                row.value,
                CounterUnit::Count,
                MeasurementScope::Database,
                "fixture",
                "count",
            )],
            properties: BTreeMap::new(),
        };
        Ok(SnapshotRecord {
            source: row.source.into(),
            complete_scope: row.complete_scope,
            record: TelemetryRecord::Operational(OperationalObservation {
                source: row.source.into(),
                key: counters.key()?,
                fact: OperationalFact::CounterSet(Box::new(counters)),
            }),
        })
    }
    async fn recheck(&mut self, _: &CollectionDescription) -> Result<SnapshotRecheck> {
        self.called("recheck");
        Ok(SnapshotRecheck {
            unchanged: !self.changed,
            observed_until: UNTIL.into(),
            scopes: std::mem::take(&mut self.scopes),
        })
    }
    fn query_count(&self) -> u32 {
        0
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> SourceShutdown {
        self.called("close");
        self.shutdown
    }
}
fn request() -> CollectionRequest {
    CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
        budget: CollectionBudget { page_rows: 1, max_snapshot_rows: 2, ..Default::default() },
    }
}
fn source(reader: Reader) -> SnapshotSource<Reader> {
    SnapshotSource::new(reader, &request()).unwrap()
}
async fn read_empty(source: &mut SnapshotSource<Reader>) {
    source.describe().await.unwrap();
    assert!(matches!(source.read_page(STREAM, None).await.unwrap(), CollectionRead::Page { next_cursor: None, .. }));
}

#[tokio::test]
async fn construction_is_offline_and_finish_requires_native_eof_before_recheck() {
    let reader = Reader::new([batch(&[u64::MAX], false), batch(&[9_007_199_254_740_993], true)]);
    let calls = reader.calls.clone();
    let mut source = source(reader);
    assert!(calls.lock().unwrap().is_empty());
    source.describe().await.unwrap();
    assert_eq!(*calls.lock().unwrap(), ["describe"]);
    let CollectionRead::Page { records, next_cursor } = source.read_page(STREAM, None).await.unwrap() else { panic!() };
    assert_eq!(next_cursor.as_deref(), Some("1"));
    assert!(serde_json::to_string(&records).unwrap().contains(&u64::MAX.to_string()));
    let CollectionRead::Page { records, next_cursor } = source.read_page(STREAM, Some("1")).await.unwrap() else {
        panic!()
    };
    assert!(next_cursor.is_none());
    assert!(serde_json::to_string(&records).unwrap().contains("9007199254740993"));
    let SourceCompletion::Snapshot(completion) = source.finish().await.unwrap() else { panic!() };
    assert_eq!(completion.records, 2);
    assert_eq!(completion.sources[0].records, 2);
    assert_eq!(*calls.lock().unwrap(), ["describe", "begin", "next", "next", "recheck"]);
    assert!(source.finish().await.is_err());
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn an_empty_complete_source_is_distinct_from_not_applicable() {
    for scope in [SnapshotScope::complete("fixture"), SnapshotScope::NotApplicable { source: "fixture".into() }] {
        let expected = if matches!(scope, SnapshotScope::NotApplicable { .. }) {
            SourceState::NotApplicable
        } else {
            SourceState::Empty
        };
        let mut reader = Reader::new([batch(&[], true)]);
        reader.scopes = vec![scope];
        let mut source = source(reader);
        read_empty(&mut source).await;
        let SourceCompletion::Snapshot(completion) = source.finish().await.unwrap() else { panic!() };
        assert_eq!(completion.sources[0].state, expected);
        assert_eq!(completion.records, 0);
    }
}

#[tokio::test]
async fn invalid_cursors_and_streams_never_start_or_replace_native_work() {
    for (stream, cursor) in [(STREAM, Some("1")), (TelemetryStream::SettingObservations, None)] {
        let mut source = source(Reader::new([]));
        source.describe().await.unwrap();
        assert!(source.read_page(stream, cursor).await.is_err());
        assert_eq!(*source.reader().calls.lock().unwrap(), ["describe"]);
        assert!(source.read_page(STREAM, None).await.is_err());
    }
    let mut source = source(Reader::new([batch(&[1], false)]));
    source.describe().await.unwrap();
    source.read_page(STREAM, None).await.unwrap();
    assert!(source.read_page(STREAM, None).await.is_err());
    assert!(source.finish().await.is_err());
}

#[tokio::test]
async fn a_late_native_error_keeps_earlier_pages_provisional_and_preserves_shutdown_uncertainty() {
    let mut reader = Reader::new([batch(&[1], false), Err(invalid_source())]);
    reader.shutdown = SourceShutdown::Unconfirmed;
    let mut source = source(reader);
    source.describe().await.unwrap();
    source.read_page(STREAM, None).await.unwrap();
    assert!(source.read_page(STREAM, Some("1")).await.is_err());
    assert!(source.finish().await.is_err());
    assert!(!source.reader().calls.lock().unwrap().contains(&"recheck"));
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert!(source.describe().await.is_err());
}

#[tokio::test]
async fn total_row_limits_are_independent_of_native_page_boundaries() {
    let mut source = source(Reader::new([batch(&[1], false), batch(&[2], false), batch(&[3], true)]));
    source.describe().await.unwrap();
    source.read_page(STREAM, None).await.unwrap();
    source.read_page(STREAM, Some("1")).await.unwrap();
    let error = source.read_page(STREAM, Some("2")).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    assert!(source.finish().await.is_err());
}

#[tokio::test]
async fn final_identity_changes_invalidate_the_observation() {
    let mut reader = Reader::new([batch(&[], true)]);
    reader.changed = true;
    let mut source = source(reader);
    read_empty(&mut source).await;
    assert!(matches!(source.finish().await.unwrap(), SourceCompletion::Changed));
}

#[tokio::test]
async fn unavailable_sources_are_explicit_and_cannot_hide_observed_rows() {
    let mut reader = Reader::new([batch(&[], true)]);
    reader.scopes =
        vec![SnapshotScope::Unavailable { source: "fixture".into(), error_code: ErrorCode::PermissionDenied }];
    let mut empty = source(reader);
    read_empty(&mut empty).await;
    let SourceCompletion::Snapshot(completion) = empty.finish().await.unwrap() else { panic!() };
    assert_eq!(completion.sources[0].state, SourceState::Unavailable);
    assert_eq!(completion.sources[0].error_code, Some(ErrorCode::PermissionDenied));
    assert!(!completion.sources[0].complete_scope);

    let mut reader = Reader::new([batch(&[1], true)]);
    reader.scopes =
        vec![SnapshotScope::Unavailable { source: "fixture".into(), error_code: ErrorCode::PermissionDenied }];
    let mut observed = source(reader);
    read_empty(&mut observed).await;
    assert!(observed.finish().await.is_err());
}

#[tokio::test]
async fn undeclared_native_sources_and_duplicate_completion_sources_cannot_publish() {
    let mut unexpected = row(1);
    unexpected.source = "unexpected";
    let mut source = source(Reader::new([Ok(ReadBatch { rows: vec![unexpected], complete: true })]));
    source.describe().await.unwrap();
    assert!(source.read_page(STREAM, None).await.is_err());
    assert!(source.finish().await.is_err());
    let mut reader = Reader::new([batch(&[], true)]);
    reader.scopes.push(SnapshotScope::complete("fixture"));
    let mut duplicate = SnapshotSource::new(reader, &request()).unwrap();
    read_empty(&mut duplicate).await;
    assert!(duplicate.finish().await.is_err());
}

#[tokio::test]
async fn partial_native_scope_survives_an_otherwise_successful_read() {
    let mut partial = row(1);
    partial.complete_scope = false;
    let mut source = source(Reader::new([Ok(ReadBatch { rows: vec![partial], complete: true })]));
    source.describe().await.unwrap();
    source.read_page(STREAM, None).await.unwrap();
    let SourceCompletion::Snapshot(completion) = source.finish().await.unwrap() else { panic!() };
    assert_eq!(completion.sources[0].state, SourceState::Partial);
    assert!(!completion.sources[0].complete_scope);
}

#[tokio::test]
async fn dropping_a_pending_read_cannot_admit_another_read() {
    let mut reader = Reader::new([]);
    reader.block_begin = true;
    reader.shutdown = SourceShutdown::Unconfirmed;
    let mut source = source(reader);
    source.describe().await.unwrap();
    {
        let read = source.read_page(STREAM, None);
        tokio::pin!(read);
        assert!(futures_util::poll!(&mut read).is_pending());
    }
    assert!(source.read_page(STREAM, None).await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(*source.reader().calls.lock().unwrap(), ["describe", "begin", "close"]);
}

#[tokio::test]
async fn materialized_native_rows_honor_page_rows_and_single_record_byte_limits() {
    let mut reader = Reader::new([]);
    let request = request();
    let description = reader.describe(&request).await.unwrap();
    let records = (0..3).map(|n| reader.decode(&row(n), &description).unwrap()).collect();
    let mut buffer = SnapshotBuffer::new(records, &CollectionBudget { page_rows: 1, ..Default::default() }).unwrap();
    for index in 0..3 {
        let batch = buffer.next_batch().unwrap();
        assert_eq!(batch.rows.len(), 1);
        assert_eq!(batch.complete, index == 2);
    }
    let record = reader.decode(&row(1), &description).unwrap();
    let mut buffer =
        SnapshotBuffer::new(vec![record], &CollectionBudget { page_bytes: 1024, ..Default::default() }).unwrap();
    assert_eq!(CollectionStop::from_error(&buffer.next_batch().err().unwrap()), CollectionStop::ByteLimit);
}
