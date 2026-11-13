use super::*;
use crate::{catalog::SchemaCatalog, client::QueryRequest, response::QueryTable, schema::Schema};
use async_trait::async_trait;
use culprit_collector::{
    analytical::{
        RowProjection,
        encoding::BatchEncoder,
        evidence::*,
        projection::ScalarType,
        publication,
        test_support::{self, Blobs, Fault, Recovery},
    },
    delivery::recovery::reopen,
};
use culprit_core::{Result, telemetry::*};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};

#[derive(Default)]
struct Queries {
    replies: Mutex<VecDeque<Vec<Vec<Value>>>>,
    requests: Mutex<Vec<QueryRequest>>,
}
#[async_trait]
impl QueryTransport for Queries {
    fn destination(&self) -> &str {
        "clickhouse:test"
    }
    async fn query(&self, request: QueryRequest) -> Result<QueryTable> {
        let rows = self.replies.lock().unwrap().pop_front().expect("unexpected warehouse query");
        assert!(rows.len() <= request.row_limit);
        assert!(rows.iter().all(|row| row.len() == request.columns.len()));
        let response_bytes = serde_json::to_vec(&rows).unwrap().len();
        assert!(response_bytes <= request.byte_limit);
        let columns = request.columns.clone();
        self.requests.lock().unwrap().push(request);
        Ok(QueryTable { columns, rows, response_bytes })
    }
}

fn returned(schema: &Schema, encoded: &Value) -> Vec<Value> {
    schema
        .columns()
        .iter()
        .map(|column| {
            let value = &encoded[&column.name];
            if column.kind == ScalarType::Timestamp && !value.is_null() {
                let nanos = chrono::DateTime::parse_from_rfc3339(value.as_str().unwrap())
                    .unwrap()
                    .timestamp_nanos_opt()
                    .unwrap();
                json!(nanos.to_string())
            } else {
                value.clone()
            }
        })
        .collect()
}

struct Fixture {
    catalog: Arc<SchemaCatalog>,
    recovery: Arc<Recovery>,
    range: culprit_collector::delivery::recovery::CommittedRange,
    reader: EvidenceReader,
    query: Arc<Queries>,
    rows: Vec<Vec<Value>>,
    publication: Vec<Value>,
}
impl Fixture {
    async fn new(source: CollectionDescription, records: Vec<TelemetryRecord>) -> Self {
        let catalog = Arc::new(SchemaCatalog::new().unwrap());
        let schema = catalog.get(TelemetryStream::OperationalSamples).unwrap();
        assert_eq!(schema.table(), "CulpritOperationalV1");
        let recovery = Arc::new(Recovery::capture("clickhouse:test", catalog.digest(), source, records, 140_000));
        let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
        assert_eq!(range.manifest().batches.len(), 1);
        assert_eq!(range.manifest().batches[0].stream, TelemetryStream::OperationalSamples);
        let indexes = Arc::new(ProjectionIndexes::new(Arc::new(Blobs::default())));
        let scratch = tempfile::tempdir().unwrap();
        let encoder = BatchEncoder::new(catalog.clone(), recovery.clone(), scratch.path()).unwrap();
        let encoded = encoder.encode(&range, &range.manifest().batches[0].batch_id).await.unwrap();
        indexes.put(&encoded.index_key, &encoded.digests).await.unwrap();
        let rows: Vec<_> = std::fs::read_to_string(encoded.path())
            .unwrap()
            .lines()
            .map(|line| returned(schema, &serde_json::from_str(line).unwrap()))
            .collect();
        let publication_schema = catalog.publication();
        let values = publication_schema.project(&publication::input(catalog.as_ref(), &range).unwrap()).unwrap();
        let publication = returned(
            publication_schema,
            &serde_json::from_slice(&publication_schema.encode(&values).unwrap()).unwrap(),
        );
        let query = Arc::new(Queries::default());
        *recovery.fault.lock().unwrap() = Fault::Stall;
        let reader = super::new(query.clone(), indexes).unwrap();
        Self { catalog, recovery, range, reader, query, rows, publication }
    }
    fn replies(&self, pages: Vec<Vec<Vec<Value>>>) {
        let mut replies = self.query.replies.lock().unwrap();
        replies.push_back(vec![self.publication.clone()]);
        replies.push_back(vec![vec![json!(0)]]);
        replies.extend(pages);
    }
}

#[tokio::test]
async fn page_wait_reads_preserve_original_requests_mapping_failures_and_exact_native_references() {
    for (source, records) in test_support::page_wait_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let result = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::OperationalSamples],
                EvidenceReadBudget { page_rows: 1, ..Default::default() },
            )
            .await
            .unwrap();
        test_support::assert_page_wait_capture(&result, &source, &records);
        assert_eq!(result.metrics().pages as usize, records.len());
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        assert_eq!(schema.table(), "CulpritOperationalV1");
        if let Some(first) = fixture.rows.first() {
            let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
            let mut tampered = first.clone();
            let mut details: Value = serde_json::from_str(tampered[column].as_str().unwrap()).unwrap();
            details["target"]["header"]["page_id"] = json!(999);
            tampered[column] = json!(serde_json::to_string(&details).unwrap());
            fixture.replies(vec![vec![tampered]]);
            let error = fixture
                .reader
                .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default())
                .await
                .unwrap_err();
            assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
        }
    }
}

#[tokio::test]
async fn memory_participants_preserve_scope_exact_values_unknowns_and_zero_row_coverage() {
    for (source, records) in
        test_support::request_memory_evidence_cases().into_iter().chain(test_support::session_memory_evidence_cases())
    {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let result = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::OperationalSamples],
                EvidenceReadBudget { page_rows: 1, ..Default::default() },
            )
            .await
            .unwrap();
        test_support::assert_memory_participants_capture(&result, &source, &records);
        assert_eq!(result.metrics().pages as usize, records.len());
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        if let Some(first) = fixture.rows.first() {
            let column = schema.columns().iter().position(|column| column.name == "FactKind").unwrap();
            let mut tampered = first.clone();
            tampered[column] = json!("unproven-request-owner");
            fixture.replies(vec![vec![tampered]]);
            let error = fixture
                .reader
                .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default())
                .await
                .unwrap_err();
            assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
        }
    }
}

#[tokio::test]
async fn native_failure_formats_keep_exact_context_partial_coverage_and_proof_membership() {
    for (source, records) in test_support::native_failure_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let result = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::OperationalSamples],
                EvidenceReadBudget { page_rows: 1, ..Default::default() },
            )
            .await
            .unwrap();
        assert!(result.is_read_complete(), "{:?}", result.gaps());
        assert_eq!(result.source(), &source);
        assert_eq!(result.records().len(), records.len());
        assert_eq!(result.metrics().pages as usize, records.len());
        assert_ne!(result.records()[0].row().record_id, result.records()[1].row().record_id);
        let snapshot = source.observation.snapshot().unwrap();
        assert_eq!(snapshot.dataset, SnapshotDataset::NativeFailures);
        assert_ne!(source.source, snapshot.sources[0].source);
        assert!(source.target.database().is_none());
        for (index, native) in records.iter().enumerate() {
            let expected = ExportedRecord::new(&source, result.capture(), native.clone()).unwrap();
            assert_eq!(result.records()[index].row(), &expected);
            assert_eq!(result.records()[index].row_ordinal() as usize, index + 1);
        }
        assert!(!serde_json::to_string(result.records()).unwrap().contains("private-pg-log-canary"));
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "FactKind").unwrap();
        let mut tampered = fixture.rows[0].clone();
        tampered[column] = json!("unproven-native-format");
        fixture.replies(vec![vec![tampered]]);
        let error = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default())
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}

#[tokio::test]
async fn retained_memory_failures_use_exact_operational_readback_without_inventing_time_or_ownership() {
    let (source, records) = test_support::memory_failure_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
    let Fixture { catalog, recovery, range, reader, query, rows, publication } = fixture;
    let schema = catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let budget = EvidenceReadBudget { page_rows: 1, ..Default::default() };
    let result = reader.read_capture(&range, &[EvidenceFamily::OperationalSamples], budget).await.unwrap();
    assert!(result.is_read_complete(), "{:?}", result.gaps());
    assert_eq!(result.source(), &source);
    assert_eq!(result.records().len(), 2);
    assert_eq!(result.metrics().pages, 2);
    assert_ne!(result.records()[0].row().record_id, result.records()[1].row().record_id);
    let CollectionTarget::Shared(resource) = &result.source().target else { panic!("shared native pool") };
    assert_eq!(resource.kind, SharedResourceKind::Pool);
    for (index, native) in records.iter().enumerate() {
        let expected = ExportedRecord::new(&source, result.capture(), native.clone()).unwrap();
        let actual = &result.records()[index];
        assert_eq!(actual.row(), &expected);
        assert_eq!(actual.row_ordinal() as usize, index + 1);
        let TelemetryRecord::Operational(observation) = &actual.row().record else { panic!() };
        let OperationalFact::MssqlOutOfMemory(row) = &observation.fact else { panic!() };
        assert_eq!(row.position as usize, index);
        assert_eq!(row.event_time_without_offset, "2020-01-01T10:30:00.003");
        assert_eq!(row.available_physical_memory_mb, i32::MIN);
        assert!(row.initial_job_object_memory_limit_mb.is_none() && row.top_resource_pools.is_none());
        assert_eq!(row.oom_resource_pools.as_ref().unwrap().raw, "null");
        assert_eq!(
            row.possible_leaked_memory_clerks.as_ref().unwrap().qualification,
            MemoryFailureJsonQualification::Unparsed
        );
        let details = serde_json::to_value(row).unwrap();
        assert!(details.get("query").is_none() && details.get("session").is_none());
        assert!(details.get("database").is_none() && details.get("event_time").is_none());
    }
    assert!(matches!(*recovery.fault.lock().unwrap(), Fault::Stall));
    assert!(query.replies.lock().unwrap().is_empty());
    {
        let requests = query.requests.lock().unwrap();
        assert!(requests.iter().all(|request| !request.text.contains(&range.manifest().range_id)));
        assert!(requests[2].parameters.contains_key("range") && requests[2].parameters.contains_key("batch"));
    }

    let details_column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let mut tampered = rows[0].clone();
    let mut details: Value = serde_json::from_str(tampered[details_column].as_str().unwrap()).unwrap();
    details["possible_leaked_memory_clerks"]["qualification"] = json!("parsed");
    tampered[details_column] = json!(serde_json::to_string(&details).unwrap());
    query.replies.lock().unwrap().extend([vec![publication], vec![vec![json!(0)]], vec![tampered]]);
    let error = reader.read_capture(&range, &[EvidenceFamily::OperationalSamples], budget).await.unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}
