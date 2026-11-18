use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Calls {
    described: AtomicUsize,
    paged: AtomicUsize,
    finished: AtomicUsize,
    closed: AtomicUsize,
}
struct Original {
    description: CollectionDescription,
    page: Option<Result<CollectionRead>>,
    calls: Arc<Calls>,
}
#[async_trait::async_trait]
impl TelemetrySource for Original {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.calls.described.fetch_add(1, Ordering::SeqCst);
        Ok(self.description.clone())
    }
    fn query_count(&self) -> u32 {
        7
    }
    async fn read_page(&mut self, _stream: TelemetryStream, _cursor: Option<&str>) -> Result<CollectionRead> {
        self.calls.paged.fetch_add(1, Ordering::SeqCst);
        self.page.take().unwrap()
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        self.calls.finished.fetch_add(1, Ordering::SeqCst);
        Err(CollectionStop::SourceChanged.error())
    }
    async fn close(&mut self) -> SourceShutdown {
        self.calls.closed.fetch_add(1, Ordering::SeqCst);
        SourceShutdown::Unconfirmed
    }
}
fn row() -> TelemetryRecord {
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    let layout = crate::retained_definitions::Layout {
        maximum_bytes: 128 * 1024,
        maximum_statement_bytes: crate::actual::retained::MAX_STATEMENT_BYTES,
        database_setting: Some(true),
    };
    let fact =
        crate::retained_definitions::decode(&crate::cached_definitions::tests::row(), &layout, Some(&source)).unwrap();
    TelemetryRecord::ObjectDefinition(
        ObjectDefinitionRecord::new(
            source.observation.snapshot().unwrap().sources[0].source.clone(),
            ObjectDefinitionFact::MssqlRetainedPlan(Box::new(fact)),
        )
        .unwrap(),
    )
}
fn original(page: Result<CollectionRead>) -> (SourceDiagnostics, Arc<Calls>) {
    let calls = Arc::new(Calls::default());
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    // Only delegation is under test; this controlled protocol identity grants no source or artifact authority.
    let capture = CaptureIdentity { observation_id: "owned-diagnostic-protocol".into(), revision: None };
    (
        SourceDiagnostics {
            inner: Box::new(Original { description: source, page: Some(page), calls: calls.clone() }),
            capture,
            description: None,
            seen: HashSet::new(),
            failure_samples: 0,
        },
        calls,
    )
}
#[tokio::test]
async fn duplicate_rows_are_observed_without_filtering_and_the_same_source_owner_alone_closes() {
    let record = row();
    let records = vec![record.clone(), record.clone()];
    let (mut spy, calls) = original(Ok(CollectionRead::Page { records: records.clone(), next_cursor: None }));
    spy.describe().await.unwrap();
    let CollectionRead::Page { records: returned, next_cursor } =
        spy.read_page(TelemetryStream::ObjectDefinitions, None).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(returned, records);
    assert!(next_cursor.is_none());
    assert_eq!(spy.failure_samples, 1);
    assert_eq!(spy.seen.len(), 1);
    assert_eq!(spy.query_count(), 7);
    assert_eq!(CollectionStop::from_error(&spy.finish().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(spy.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(calls.described.load(Ordering::SeqCst), 1);
    assert_eq!(calls.paged.load(Ordering::SeqCst), 1);
    assert_eq!(calls.finished.load(Ordering::SeqCst), 1);
    assert_eq!(calls.closed.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn canonical_export_refusals_and_failure_sample_cap_never_repair_or_replace_returned_originals() {
    let mut invalid = row();
    let TelemetryRecord::ObjectDefinition(record) = &mut invalid else { panic!() };
    record.version = "0".repeat(64);
    let records = vec![invalid; MAX_FAILURE_SAMPLES + 2];
    let (mut spy, _) = original(Ok(CollectionRead::Page { records: records.clone(), next_cursor: None }));
    spy.describe().await.unwrap();
    let CollectionRead::Page { records: returned, .. } =
        spy.read_page(TelemetryStream::ObjectDefinitions, None).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(returned, records);
    assert_eq!(spy.failure_samples, MAX_FAILURE_SAMPLES);
    assert_eq!(spy.close().await, SourceShutdown::Unconfirmed);
}
#[tokio::test]
async fn native_errors_keep_their_original_stop_and_diagnostic_facts_never_include_text_or_handles() {
    let (mut spy, _) = original(Err(CollectionStop::CancellationUnconfirmed.error()));
    spy.describe().await.unwrap();
    let error = spy.read_page(TelemetryStream::ObjectDefinitions, None).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(spy.failure_samples, 0);
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    let facts = row_facts(&source, &row());
    assert_eq!(facts["kind"], "mssql-retained-plan");
    assert_eq!(facts["record_valid"], true);
    assert_eq!(facts["source_fit"], true);
    let encoded = serde_json::to_string(&facts).unwrap();
    for private in ["private-value", "SELECT", "ShowPlanXML", "0x", "plan_handle", "sql_handle"] {
        assert!(!encoded.contains(private));
    }
    assert!(encoded.len() < 4096);
    assert_eq!(spy.close().await, SourceShutdown::Unconfirmed);
}
