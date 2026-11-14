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
        Self::new_for_stream(source, records, TelemetryStream::OperationalSamples).await
    }
    async fn new_for_stream(
        source: CollectionDescription,
        records: Vec<TelemetryRecord>,
        stream: TelemetryStream,
    ) -> Self {
        let catalog = Arc::new(SchemaCatalog::new().unwrap());
        let schema = catalog.get(stream).unwrap();
        let recovery = Arc::new(Recovery::capture("clickhouse:test", catalog.digest(), source, records, 140_000));
        let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
        let indexes = Arc::new(ProjectionIndexes::new(Arc::new(Blobs::default())));
        let scratch = tempfile::tempdir().unwrap();
        let encoder = BatchEncoder::new(catalog.clone(), recovery.clone(), scratch.path()).unwrap();
        let batch = range.manifest().batches.iter().find(|batch| batch.stream == stream).unwrap();
        let encoded = encoder.encode(&range, &batch.batch_id).await.unwrap();
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
async fn completed_spill_pages_preserve_exact_native_cache_counters_and_unknown_values() {
    for (source, records) in test_support::cached_runtime_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.replies(vec![fixture.rows.clone()]);
        let captured = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default())
            .await
            .unwrap();
        assert!(captured.is_read_complete(), "{:?}", captured.gaps());
        assert_eq!(captured.records().len(), 1);
        let expected = ExportedRecord::new(&source, captured.capture(), records[0].clone()).unwrap();
        assert_eq!(captured.records()[0].row(), &expected);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn changed_spill_pages_fail_the_original_native_projection_digest() {
    let (source, records) = test_support::cached_runtime_evidence_cases().pop().unwrap();
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let mut rows = fixture.rows.clone();
    let mut native: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
    native["counters"]["spilled-pages"]["total"] = json!("0");
    rows[0][column] = json!(serde_json::to_string(&native).unwrap());
    fixture.replies(vec![rows]);
    let error = fixture
        .reader
        .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default())
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}

#[tokio::test]
async fn native_variant_metadata_keeps_child_identity_without_a_parent_runtime_or_plan_payload() {
    for with_family in [false, true] {
        let (source, records) =
            if with_family { test_support::query_variant_evidence() } else { test_support::query_evidence() };
        let expected = records.iter().find(|row| matches!(row, TelemetryRecord::QueryDefinition(_))).unwrap().clone();
        let fixture = Fixture::new_for_stream(source.clone(), records, TelemetryStream::QueryDefinitions).await;
        fixture.replies(vec![fixture.rows.clone()]);
        let captured = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::QueryDefinitions], EvidenceReadBudget::default())
            .await
            .unwrap();
        assert!(captured.is_read_complete(), "{:?}", captured.gaps());
        assert_eq!(captured.records().len(), 1);
        let expected = ExportedRecord::new(&source, captured.capture(), expected).unwrap();
        assert_eq!(captured.records()[0].row(), &expected);
        let TelemetryRecord::QueryDefinition(query) = &captured.records()[0].row().record else { panic!() };
        assert_eq!(query.variant_family.is_some(), with_family);
        if let Some(family) = &query.variant_family {
            assert_eq!(family.variant, query.identity);
            assert_ne!(family.parent, family.variant);
            assert_eq!(family.dispatcher.query, family.parent);
        }
    }
}

#[tokio::test]
async fn changed_native_variant_relationships_cannot_pass_the_committed_projection() {
    let (source, records) = test_support::query_variant_evidence();
    let fixture = Fixture::new_for_stream(source, records, TelemetryStream::QueryDefinitions).await;
    let schema = fixture.catalog.get(TelemetryStream::QueryDefinitions).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "QueryVariantFamily").unwrap();
    for pointer in
        ["/variant/qualifiers/context", "/parent/query_id", "/dispatcher/plan_id", "/dispatcher/qualifiers/generation"]
    {
        let mut rows = fixture.rows.clone();
        let mut family: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *family.pointer_mut(pointer).unwrap() = json!("another-native-incarnation");
        rows[0][column] = json!(serde_json::to_string(&family).unwrap());
        fixture.replies(vec![rows]);
        let error = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::QueryDefinitions], EvidenceReadBudget::default())
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}

#[tokio::test]
async fn query_runtime_keeps_typed_temporary_space_units_averages_and_exact_native_execution_counts() {
    let (source, records) = test_support::query_evidence();
    let expected =
        records.iter().filter(|row| row.stream() == TelemetryStream::QueryRuntime).cloned().collect::<Vec<_>>();
    let fixture = Fixture::new_for_stream(source.clone(), records, TelemetryStream::QueryRuntime).await;
    fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
    let result = fixture
        .reader
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::QueryRuntime],
            EvidenceReadBudget { page_rows: 1, ..Default::default() },
        )
        .await
        .unwrap();
    assert!(result.is_read_complete(), "{:?}", result.gaps());
    assert_eq!(result.source(), &source);
    assert_eq!(result.records().len(), expected.len());
    assert_eq!(result.metrics().pages as usize, expected.len());
    for row in expected {
        let expected = ExportedRecord::new(&source, result.capture(), row).unwrap();
        let actual = result.records().iter().find(|row| row.row() == &expected).unwrap();
        let TelemetryRecord::QueryRuntime(runtime) = &actual.row().record else { panic!() };
        let space = runtime.metric(&RuntimeMetricKey::TemporaryStorage).unwrap();
        assert_eq!(space.unit, RuntimeMetricUnit::Pages { bytes_per_page: 16384 });
        assert_eq!(space.average, 1.25);
        assert_eq!(runtime.executions, "9007199254740993");
    }
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    let schema = fixture.catalog.get(TelemetryStream::QueryRuntime).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Metrics").unwrap();
    for (pointer, replacement) in [("/2/average", json!(99.0)), ("/2/unit/bytes_per_page", json!(8192))] {
        let mut rows = fixture.rows.clone();
        let mut metrics: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *metrics.pointer_mut(pointer).unwrap() = replacement;
        rows[0][column] = json!(serde_json::to_string(&metrics).unwrap());
        fixture.replies(vec![rows]);
        let error = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::QueryRuntime], EvidenceReadBudget::default())
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}

#[tokio::test]
async fn runtime_distributions_preserve_native_bounds_independent_clocks_and_per_source_coverage() {
    for (source, records) in test_support::runtime_distribution_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::OperationalSamples],
                EvidenceReadBudget { page_rows: 1, ..Default::default() },
            )
            .await
            .unwrap();
        test_support::assert_runtime_distribution_capture(&capture, &source, &records);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        if let Some(first) = fixture.rows.first() {
            let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
            let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
            for (pointer, value) in [
                ("/observed_at", json!("2020-01-01T12:00:00.900000000Z")),
                ("/distribution/p99/upper/value", json!("1")),
            ] {
                let mut tampered = first.clone();
                let mut details: Value = serde_json::from_str(tampered[column].as_str().unwrap()).unwrap();
                let Some(field) = details.pointer_mut(pointer) else { continue };
                *field = value;
                tampered[column] = json!(serde_json::to_string(&details).unwrap());
                fixture.replies(vec![vec![tampered]]);
                let error = fixture
                    .reader
                    .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default())
                    .await
                    .unwrap_err();
                assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
                assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
            }
        }
    }
}

#[tokio::test]
async fn task_space_reads_preserve_exact_counters_context_checks_and_empty_source_coverage() {
    for (source, records) in test_support::task_space_evidence_cases() {
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
        test_support::assert_task_space_capture(&result, &source, &records);
        assert_eq!(result.metrics().pages as usize, records.len());
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        assert_eq!(schema.table(), "CulpritOperationalV1");
        if let Some(first) = fixture.rows.first() {
            let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
            for (pointer, value) in
                [("/task/execution_context_id", json!("99")), ("/after_check/state", json!("changed"))]
            {
                let mut tampered = first.clone();
                let mut details: Value = serde_json::from_str(tampered[column].as_str().unwrap()).unwrap();
                *details.pointer_mut(pointer).unwrap() = value;
                tampered[column] = json!(serde_json::to_string(&details).unwrap());
                fixture.replies(vec![vec![tampered]]);
                let error = fixture
                    .reader
                    .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default())
                    .await
                    .unwrap_err();
                assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
                assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
            }
        }
    }
}

#[tokio::test]
async fn statement_work_points_preserve_typed_native_counts_zeros_and_read_coverage() {
    for (source, records) in test_support::statement_work_evidence_cases() {
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
        test_support::assert_statement_work_capture(&result, &source, &records);
        assert_eq!(result.metrics().pages as usize, records.len());
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        if let Some(first) = fixture.rows.first() {
            let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
            let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
            let mut tampered = first.clone();
            let mut details: Value = serde_json::from_str(tampered[column].as_str().unwrap()).unwrap();
            details["totals"][0]["total"]["value"] = json!("1");
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
            let original: Value = serde_json::from_str(first[column].as_str().unwrap()).unwrap();
            for details in test_support::page_wait_tamper_cases(&original) {
                let mut tampered = first.clone();
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
