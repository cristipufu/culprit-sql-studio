use super::*;
use crate::test_support::{Blobs, Fault, Queries, Recovery, Reply, returned};
use culprit_collector::{
    analytical::{encoding::BatchEncoder, evidence::*, publication, test_support},
    delivery::recovery::reopen,
};
use culprit_core::telemetry::*;
use serde_json::json;

mod observations;
mod queries;
mod waits;

struct Fixture {
    query: Arc<Queries>,
    catalog: Arc<SchemaCatalog>,
    recovery: Arc<Recovery>,
    range: CommittedRange,
    indexes: Arc<ProjectionIndexes>,
    rows: Vec<Vec<Value>>,
    publication: Vec<Value>,
}
impl Fixture {
    async fn new(source: CollectionDescription, records: Vec<TelemetryRecord>) -> Self {
        let catalog = Arc::new(SchemaCatalog::new().unwrap());
        let recovery = Arc::new(Recovery::capture("adx:test", catalog.digest(), source, records, 50_000));
        let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
        let indexes = Arc::new(ProjectionIndexes::new(Arc::new(Blobs::default())));
        let scratch = tempfile::tempdir().unwrap();
        let encoder = BatchEncoder::new(catalog.clone(), recovery.clone(), scratch.path()).unwrap();
        let mut rows = vec![];
        for batch in &range.manifest().batches {
            let encoded = encoder.encode(&range, &batch.batch_id).await.unwrap();
            indexes.put(&encoded.index_key, &encoded.digests).await.unwrap();
            let schema = catalog.get(batch.stream).unwrap();
            for line in std::fs::read_to_string(encoded.path()).unwrap().lines() {
                rows.push(returned(schema, &serde_json::from_str(line).unwrap()));
            }
        }
        let schema = catalog.publication();
        let input = publication::input(catalog.as_ref(), &range).unwrap();
        let values = schema.project(&input).unwrap();
        let input = serde_json::from_slice(&schema.encode(&values).unwrap()).unwrap();
        let publication = returned(schema, &input);
        *recovery.fault.lock().unwrap() = Fault::Stall;
        Self { query: Arc::new(Queries::default()), catalog, recovery, range, indexes, rows, publication }
    }
    fn reader(&self) -> EvidenceReader {
        super::new(self.query.clone(), self.indexes.clone()).unwrap()
    }
    fn replies(&self, pages: Vec<Vec<Vec<Value>>>) {
        let mut replies = self.query.replies.lock().unwrap();
        replies.push_back(Reply::Rows(vec![self.publication.clone()]));
        replies.push_back(Reply::Audit(0));
        replies.extend(pages.into_iter().map(Reply::Rows));
    }
    fn rows_for(&self, family: EvidenceFamily) -> Vec<Vec<Value>> {
        let Some(batch) = self.range.manifest().batches.iter().find(|batch| batch.stream == family.stream()) else {
            return vec![];
        };
        let schema = self.catalog.get(batch.stream).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "BatchId").unwrap();
        self.rows.iter().filter(|row| row[column].as_str() == Some(batch.batch_id.as_str())).cloned().collect()
    }
    fn reply_families(&self, families: &[EvidenceFamily], page_rows: usize) {
        self.replies(
            families
                .iter()
                .flat_map(|family| self.rows_for(*family).chunks(page_rows).map(<[_]>::to_vec).collect::<Vec<_>>())
                .collect(),
        );
    }
}
fn budget() -> EvidenceReadBudget {
    EvidenceReadBudget { timeout_ms: 1000, ..Default::default() }
}

#[tokio::test]
async fn read_only_pool_evidence_preserves_unknowns_and_never_rereads_native_payloads() {
    let (source, record) = test_support::pool_history();
    let fixture = Fixture::new(source, vec![record]).await;
    fixture.replies(vec![fixture.rows.clone()]);
    let result =
        fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::ResourceSamples], budget()).await.unwrap();
    assert!(result.is_read_complete());
    assert_eq!(result.source(), &fixture.range.manifest().source);
    assert_eq!(result.capture(), &fixture.range.manifest().capture);
    assert_eq!(result.metrics().queries, 3);
    assert_eq!(result.records()[0].row_ordinal(), 1);
    let TelemetryRecord::ResourceSample(sample) = &result.records()[0].row().record else { panic!() };
    assert!(sample.replica_id.is_none());
    assert_eq!(sample.metrics[0].value, Some(0.0));
    assert!(sample.metrics[1].value.is_none());
    assert_eq!(sample.metrics[2].unit, ResourceUnit::Bytes);
    assert_eq!(sample.metrics[3].scope, culprit_core::MeasurementScope::Server);
    assert_eq!(sample.metrics[3].limit_basis, ResourceLimitBasis::InstanceLimit);
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    let requests = fixture.query.requests.lock().unwrap();
    assert!(requests.iter().all(|request| !request.text.contains(&fixture.range.manifest().range_id)));
    assert!(requests[2].parameters.contains_key("pRange"));
    assert!(requests[2].parameters.contains_key("pBatch"));
}

#[tokio::test]
async fn native_counter_pages_keep_exact_values_reset_epochs_and_nullable_field_evidence() {
    let (source, records) = test_support::native_counter_sets();
    let fixture = Fixture::new(source, records).await;
    fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
    let result = fixture
        .reader()
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::OperationalSamples],
            EvidenceReadBudget { page_rows: 1, ..budget() },
        )
        .await
        .unwrap();
    assert!(result.is_read_complete());
    assert_eq!(result.records().len(), 2);
    assert_eq!(result.metrics().pages, 2);
    for record in result.records() {
        let TelemetryRecord::Operational(observation) = &record.row().record else { panic!() };
        let OperationalFact::CounterSet(set) = &observation.fact else { panic!() };
        assert_eq!(set.scope, culprit_core::MeasurementScope::Server);
        assert_eq!(set.counters[0].value, MeasurementValue::Integer("0".into()));
        assert_eq!(set.counters[1].value, MeasurementValue::Integer("9007199254740993".into()));
        assert_eq!(set.counters[1].reset, culprit_core::ResetMarker::At("2020-01-01T01:00:00.000000001Z".into()));
        assert_eq!(set.properties["native_nulls"], "committed_kb");
        assert_eq!(set.properties["unavailable_columns"], "target_kb");
    }
}

#[tokio::test]
async fn complete_readback_preserves_partial_and_unavailable_native_source_coverage() {
    let (mut source, records) = test_support::native_counter_sets();
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!() };
    let source_name = snapshot.sources[0].source.clone();
    snapshot.sources[0] = StateSourceCoverage::observed(source_name, records.len() as u32, false);
    snapshot.sources.push(StateSourceCoverage::unavailable(
        "native-restricted-source".into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    ));
    let expected_source = source.clone();
    let fixture = Fixture::new(source, records).await;
    fixture.replies(vec![fixture.rows.clone()]);
    let result =
        fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget()).await.unwrap();
    assert!(result.is_read_complete());
    assert_eq!(result.records().len(), 2);
    assert_eq!(result.source(), &expected_source);
    let SourceObservation::Snapshot(snapshot) = &result.source().observation else { panic!() };
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Partial);
    assert!(!snapshot.sources[0].complete_scope);
    assert_eq!(snapshot.sources[1].state, culprit_core::SourceState::Unavailable);
    assert_eq!(snapshot.sources[1].error_code, Some(culprit_core::ErrorCode::PermissionDenied));
}

#[tokio::test]
async fn shared_operational_reads_reconstruct_counter_sets_scalars_and_unknown_temp_space_together() {
    let (source, records) = test_support::shared_operational();
    let fixture = Fixture::new(source, records).await;
    fixture.replies(vec![fixture.rows.clone()]);
    let result =
        fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget()).await.unwrap();
    assert!(result.is_read_complete());
    assert_eq!(result.records().len(), 4);
    let mut kinds = std::collections::BTreeSet::new();
    for record in result.records() {
        let TelemetryRecord::Operational(observation) = &record.row().record else { panic!() };
        match &observation.fact {
            OperationalFact::CounterSet(set) => {
                kinds.insert("counter-set");
                assert_eq!(set.scope, culprit_core::MeasurementScope::Server);
            }
            OperationalFact::Counter(counter) => {
                kinds.insert("counter");
                assert_eq!(counter.value, MeasurementValue::Integer("0".into()));
                assert_eq!(counter.unit, CounterUnit::Count);
                assert_eq!(counter.scope, culprit_core::MeasurementScope::Server);
            }
            OperationalFact::Space(space) => {
                kinds.insert("space");
                assert_eq!(space.scope, culprit_core::FileScope::Server);
                assert!(space.used_bytes.is_none());
                assert_eq!(space.total_bytes, Some(1024.0));
                assert_eq!(space.parts[0].bytes, Some(0.0));
                assert!(space.parts[1].bytes.is_none());
            }
            _ => panic!("unexpected fixture fact"),
        }
    }
    assert_eq!(kinds, std::collections::BTreeSet::from(["counter-set", "counter", "space"]));
}

#[tokio::test]
async fn corrupt_operational_shapes_or_counter_units_cannot_hide_behind_the_native_payload_digest() {
    let (source, records) = test_support::shared_operational();
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let kind = schema.columns().iter().position(|column| column.name == "FactKind").unwrap();
    let details = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let native_digest = schema.columns().iter().position(|column| column.name == "NativePayloadDigest").unwrap();
    let counter = fixture.rows.iter().position(|row| row[kind].as_str() == Some("counter")).unwrap();
    for malformed_shape in [true, false] {
        let mut rows = fixture.rows.clone();
        let mut value: Value = serde_json::from_str(rows[counter][details].as_str().unwrap()).unwrap();
        if malformed_shape {
            value = json!({});
        } else {
            value["unit"]["kind"] = json!("invalid-native-unit");
        }
        rows[counter][details] = json!(serde_json::to_string(&value).unwrap());
        assert_eq!(rows[counter][native_digest], fixture.rows[counter][native_digest]);
        fixture.replies(vec![rows]);
        let error = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget())
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}

#[tokio::test]
async fn conflicting_rows_or_completion_heads_are_rejected_but_identical_retries_collapse() {
    let (source, record) = test_support::pool_history();
    let fixture = Fixture::new(source, vec![record]).await;
    fixture.replies(vec![vec![fixture.rows[0].clone(), fixture.rows[0].clone()]]);
    let result =
        fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::ResourceSamples], budget()).await.unwrap();
    assert!(result.is_read_complete());
    assert_eq!(result.records().len(), 1);
    let mut wrong = fixture.rows[0].clone();
    let schema = fixture.catalog.get(TelemetryStream::ResourceSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Metrics").unwrap();
    wrong[column] = json!("[]");
    fixture.replies(vec![vec![fixture.rows[0].clone(), wrong]]);
    let error =
        fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::ResourceSamples], budget()).await.unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    let mut wrong = fixture.publication.clone();
    let sequence =
        fixture.catalog.publication().columns().iter().position(|column| column.name == "CaptureSequence").unwrap();
    wrong[sequence] = json!(99);
    fixture.query.replies.lock().unwrap().push_back(Reply::Rows(vec![fixture.publication.clone(), wrong]));
    assert!(fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::ResourceSamples], budget()).await.is_err());
}

#[tokio::test]
async fn missing_rows_proofs_and_publication_are_explicit_unavailability() {
    let (source, record) = test_support::pool_history();
    let fixture = Fixture::new(source, vec![record]).await;
    fixture.query.replies.lock().unwrap().push_back(Reply::Rows(vec![]));
    let result =
        fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::ResourceSamples], budget()).await.unwrap();
    assert!(!result.is_read_complete());
    assert_eq!(result.gaps()[0].reason, EvidenceGapReason::NotPublished);
    fixture.replies(vec![vec![]]);
    let result =
        fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::ResourceSamples], budget()).await.unwrap();
    assert_eq!(result.gaps()[0].reason, EvidenceGapReason::MissingRows);
    fixture.replies(vec![]);
    let empty_proofs = Arc::new(ProjectionIndexes::new(Arc::new(Blobs::default())));
    let result = super::new(fixture.query.clone(), empty_proofs)
        .unwrap()
        .read_capture(&fixture.range, &[EvidenceFamily::ResourceSamples], budget())
        .await
        .unwrap();
    assert_eq!(result.gaps()[0].reason, EvidenceGapReason::ProjectionProofUnavailable);
}

#[tokio::test]
async fn native_nanosecond_precision_cannot_be_reconstructed_from_a_verified_rounded_projection() {
    let (source, mut record) = test_support::pool_history();
    let TelemetryRecord::ResourceSample(sample) = &mut record else { panic!() };
    sample.at = "2020-01-01T06:00:15.000000001Z".into();
    let fixture = Fixture::new(source, vec![record]).await;
    fixture.replies(vec![fixture.rows.clone()]);
    let result =
        fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::ResourceSamples], budget()).await.unwrap();
    assert!(!result.is_read_complete());
    assert!(result.records().is_empty());
    assert_eq!(result.gaps()[0].reason, EvidenceGapReason::NativeRepresentationRequired);
}

#[tokio::test]
async fn row_budgets_are_checked_before_querying_and_query_failures_never_become_empty_success() {
    let (source, records) = test_support::native_counter_sets();
    let fixture = Fixture::new(source, records).await;
    let error = fixture
        .reader()
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::OperationalSamples],
            EvidenceReadBudget { max_rows: 1, ..budget() },
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert!(fixture.query.requests.lock().unwrap().is_empty());
    fixture.query.replies.lock().unwrap().push_back(Reply::Failure);
    assert!(
        fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget()).await.is_err()
    );
}

#[tokio::test]
async fn verified_large_details_require_recovery_instead_of_silently_dropping_counter_fields() {
    let (mut source, mut records) = test_support::native_counter_sets();
    records.truncate(1);
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!() };
    snapshot.records = 1;
    snapshot.sources[0].records = 1;
    let TelemetryRecord::Operational(observation) = &mut records[0] else { panic!() };
    let OperationalFact::CounterSet(set) = &mut observation.fact else { panic!() };
    set.counters = (0..128)
        .map(|index| {
            NativeCounterSample::gauge(
                &format!("field_{index}"),
                0,
                CounterUnit::Count,
                culprit_core::MeasurementScope::Server,
                &"o".repeat(512),
                &"n".repeat(512),
            )
        })
        .collect();
    let fixture = Fixture::new(source, records).await;
    fixture.replies(vec![fixture.rows.clone()]);
    let result =
        fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget()).await.unwrap();
    assert!(!result.is_read_complete());
    assert!(result.records().is_empty());
    assert_eq!(result.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
}

#[tokio::test]
async fn a_stalled_read_has_a_whole_operation_deadline_and_cannot_claim_empty_coverage() {
    struct PendingQuery;
    #[async_trait::async_trait]
    impl crate::client::QueryTransport for PendingQuery {
        fn destination(&self) -> &str {
            "adx:test"
        }
        async fn query(&self, _: crate::client::QueryRequest) -> Result<crate::response::QueryTable> {
            std::future::pending().await
        }
    }
    let (source, record) = test_support::pool_history();
    let fixture = Fixture::new(source, vec![record]).await;
    let reader = super::new(Arc::new(PendingQuery), fixture.indexes.clone()).unwrap();
    let error = reader
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::ResourceSamples],
            EvidenceReadBudget { timeout_ms: 10, ..budget() },
        )
        .await
        .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
}

#[tokio::test]
async fn publication_audit_and_data_queries_share_one_cumulative_wire_budget() {
    let (source, record) = test_support::pool_history();
    let fixture = Fixture::new(source, vec![record]).await;
    let publication_bytes = serde_json::to_vec(&vec![fixture.publication.clone()]).unwrap().len();
    let audit_bytes = serde_json::to_vec(&vec![vec![json!(0)]]).unwrap().len();
    let data_bytes = serde_json::to_vec(&fixture.rows).unwrap().len();
    fixture.replies(vec![fixture.rows.clone()]);
    let limit = 1024 * 1024;
    let result = fixture
        .reader()
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::ResourceSamples],
            EvidenceReadBudget { page_bytes: limit as u32, max_bytes: limit as u64, ..budget() },
        )
        .await
        .unwrap();
    assert!(result.is_read_complete());
    assert_eq!(result.metrics().response_bytes, (publication_bytes + audit_bytes + data_bytes) as u64);
    let requests = fixture.query.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].byte_limit, limit);
    assert_eq!(requests[1].byte_limit, limit - publication_bytes);
    assert_eq!(requests[2].byte_limit, limit - publication_bytes - audit_bytes);
}

#[tokio::test]
async fn an_oversized_decoded_response_is_rejected_even_if_a_transport_ignores_its_limit() {
    let (source, record) = test_support::pool_history();
    let fixture = Fixture::new(source, vec![record]).await;
    let mut oversized = fixture.publication.clone();
    let details =
        fixture.catalog.publication().columns().iter().position(|column| column.name == "SourceDescription").unwrap();
    oversized[details] = json!("x".repeat(1024 * 1024));
    fixture.query.replies.lock().unwrap().push_back(Reply::Rows(vec![oversized]));
    let error = fixture
        .reader()
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::ResourceSamples],
            EvidenceReadBudget { page_bytes: 1024 * 1024, max_bytes: 1024 * 1024, ..budget() },
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    let requests = fixture.query.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].byte_limit, 1024 * 1024);
}
