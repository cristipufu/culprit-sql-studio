use super::*;
use crate::{
    analytical::{
        ProjectionCatalog, RowProjection,
        catalog::PhysicalCatalog,
        envelope::Envelope,
        indexes::{ExpectedRow, IndexKey, ProjectionIndexes},
        projection, publication,
        read_store::{AnalyticalReadPage, AnalyticalReadStore},
        test_support::{self, Blobs, Fault, JsonProjection, Recovery},
    },
    delivery::{evidence::RowReceipt, recovery::reopen},
};
use async_trait::async_trait;
use culprit_core::{ErrorDetail, Result, telemetry::*};
use serde_json::{Map, Value};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Store {
    catalog: Arc<PhysicalCatalog<JsonProjection>>,
    rows: Mutex<BTreeMap<String, Vec<Vec<Value>>>>,
    published: AtomicBool,
    queries: AtomicUsize,
}
#[async_trait]
impl AnalyticalReadStore for Store {
    fn destination(&self) -> &str {
        "fixture:hydration"
    }
    fn catalog(&self) -> Arc<dyn ProjectionCatalog> {
        self.catalog.clone()
    }
    fn logical_values(&self, stream: TelemetryStream, row: &[Value]) -> Result<Map<String, Value>> {
        self.catalog.get(stream)?.logical_values(row)
    }
    async fn audit_membership(&self, _: &CommittedRange, _: usize) -> Result<usize> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Ok(1)
    }
    async fn rows(
        &self,
        _: &CommittedRange,
        batch: &str,
        after: usize,
        until: usize,
        _: usize,
    ) -> Result<AnalyticalReadPage> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        let rows = self.rows.lock().unwrap()[batch].iter().skip(after).take(until - after).cloned().collect::<Vec<_>>();
        Ok(AnalyticalReadPage { response_bytes: serde_json::to_vec(&rows).unwrap().len(), rows })
    }
    async fn publication_rows(&self, range: &CommittedRange, _: usize) -> Result<AnalyticalReadPage> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        let schema = self.catalog.publication();
        let rows = if self.published.load(Ordering::SeqCst) {
            vec![physical(schema, &schema.project(&publication::input(self.catalog.as_ref(), range)?)?)]
        } else {
            Vec::new()
        };
        Ok(AnalyticalReadPage { response_bytes: serde_json::to_vec(&rows).unwrap().len(), rows })
    }
}

fn physical(schema: &JsonProjection, values: &[Value]) -> Vec<Value> {
    let row: Map<String, Value> = serde_json::from_slice(&schema.encode(values).unwrap()).unwrap();
    schema.columns().iter().map(|column| row[&column.name].clone()).collect()
}

struct Fixture {
    range: CommittedRange,
    recovery: Recovery,
    store: Arc<Store>,
    proofs: Arc<Blobs>,
    reader: EvidenceReader,
}
impl Fixture {
    async fn new(source: CollectionDescription, records: Vec<TelemetryRecord>) -> Self {
        let catalog = Arc::new(PhysicalCatalog::<JsonProjection>::new().unwrap());
        let recovery = Recovery::capture("fixture:hydration", catalog.digest(), source, records.clone(), 201_001);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let proofs = Arc::new(Blobs::default());
        let indexes = Arc::new(ProjectionIndexes::new(proofs.clone()));
        let mut returned = BTreeMap::new();
        for batch in &range.manifest().batches {
            let schema = catalog.get(batch.stream).unwrap();
            let envelope = Envelope::committed(&range, &batch.batch_id).unwrap();
            let mut rows = Vec::new();
            let mut expected = Vec::new();
            for (index, record) in records.iter().filter(|record| record.stream() == batch.stream).enumerate() {
                let row =
                    ExportedRecord::new(&range.manifest().source, &range.manifest().capture, record.clone()).unwrap();
                let values = schema.project(&envelope.input(index + 1, &row).unwrap()).unwrap();
                expected.push(
                    ExpectedRow::new(projection::digest(&values).unwrap(), schema.encode(&values).unwrap().len())
                        .unwrap(),
                );
                rows.push(physical(schema, &values));
            }
            indexes
                .put(&IndexKey::for_batch(&range, &batch.batch_id, schema.digest()).unwrap(), &expected)
                .await
                .unwrap();
            returned.insert(batch.batch_id.clone(), rows);
        }
        let store = Arc::new(Store {
            catalog,
            rows: Mutex::new(returned),
            published: AtomicBool::new(true),
            queries: AtomicUsize::new(0),
        });
        let reader = EvidenceReader::new(store.clone(), indexes);
        Self { range, recovery, store, proofs, reader }
    }
    async fn objects() -> Self {
        let (source, records) = test_support::object_definition_recovery_only_evidence();
        Self::new(source, records).await
    }
    async fn read(
        &self,
        families: &[EvidenceFamily],
        payloads: Option<RecoveryPayloadBudget>,
    ) -> Result<EvidenceCapture> {
        self.reader
            .read_capture(
                &self.range,
                families,
                EvidenceReadBudget { recovery_payloads: payloads, ..Default::default() },
                Some(&self.recovery),
            )
            .await
    }
    fn reads(&self) -> usize {
        self.recovery.object_reads.load(Ordering::SeqCst)
    }
}

fn gaps(range: &CommittedRange) -> EvidenceCapture {
    let manifest = range.manifest();
    EvidenceCapture {
        range_id: manifest.range_id.clone(),
        recovery_manifest_sha256: range.manifest_digest().into(),
        capture_manifest_sha256: manifest.capture_manifest_sha256.clone(),
        source: manifest.source.clone(),
        capture: manifest.capture.clone(),
        reference_coverage: manifest.reference_coverage.clone(),
        captured_rows: manifest
            .batches
            .iter()
            .map(|batch| (batch.stream, u32::try_from(batch.rows.len()).unwrap()))
            .collect(),
        collected_from: manifest.collected_from.clone(),
        collected_until: manifest.collected_until.clone(),
        families: manifest
            .batches
            .iter()
            .map(|batch| *EvidenceFamily::ALL.iter().find(|family| family.stream() == batch.stream).unwrap())
            .collect(),
        records: Vec::new(),
        gaps: manifest
            .batches
            .iter()
            .flat_map(|batch| {
                (1..=batch.rows.len()).map(|ordinal| EvidenceGap {
                    stream: Some(batch.stream),
                    batch_id: Some(batch.batch_id.clone()),
                    row_ordinal: Some(ordinal as u32),
                    reason: EvidenceGapReason::DetailInRecovery,
                })
            })
            .collect(),
        metrics: EvidenceReadMetrics::default(),
    }
}

fn two_streams() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (source, records) = test_support::query_evidence();
    let mut records = records[..2].to_vec();
    let TelemetryRecord::QueryDefinition(query) = &mut records[0] else { panic!() };
    let columns = (0..500).map(|index| format!("column_{index}")).collect::<Vec<_>>();
    let sql = format!("SELECT {} FROM native_table", columns.join(","));
    query.source_text_bytes = Some(sql.len().to_string());
    query.sql = DefinitionPayload::sql(&sql, source.target.engine());
    let TelemetryRecord::PlanDefinition(plan) = &mut records[1] else { panic!() };
    plan.plan = DefinitionPayload::plan(
        &serde_json::json!({"Plan":{"Node Type":"Seq Scan","Relation Name":"native_table","Output":columns}})
            .to_string(),
        source.target.engine(),
    );
    (source, records)
}

#[test]
fn recovery_payload_budgets_are_explicit_and_bounded() {
    assert!(EvidenceReadBudget::default().recovery_payloads.is_none());
    let defaults = RecoveryPayloadBudget::default();
    assert_eq!(defaults.max_objects, 8);
    assert_eq!(defaults.max_bytes, 64 * 1024 * 1024);
    for (max_objects, max_bytes, valid) in [
        (1, 1, true),
        (128, 256 * 1024 * 1024, true),
        (0, 1, false),
        (129, 1, false),
        (1, 0, false),
        (1, 256 * 1024 * 1024 + 1, false),
    ] {
        let budget = EvidenceReadBudget {
            recovery_payloads: Some(RecoveryPayloadBudget { max_objects, max_bytes }),
            ..Default::default()
        };
        assert_eq!(budget.validate().is_ok(), valid);
    }
}

#[tokio::test]
async fn opt_in_restores_exact_long_payloads_while_projection_only_reads_never_open_objects() {
    let fixture = Fixture::objects().await;
    *fixture.recovery.fault.lock().unwrap() = Fault::Stall;
    let projection = fixture.read(&[EvidenceFamily::ObjectDefinitions], None).await.unwrap();
    assert_eq!(fixture.reads(), 0);
    assert_eq!(projection.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
    assert!(projection.records().is_empty());
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let recovered =
        fixture.read(&[EvidenceFamily::ObjectDefinitions], Some(RecoveryPayloadBudget::default())).await.unwrap();
    assert!(recovered.is_read_complete());
    assert_eq!(fixture.reads(), 1);
    assert_eq!(recovered.metrics().recovery_objects, 1);
    assert_eq!(recovered.metrics().recovery_bytes, fixture.range.manifest().batches[0].bytes);
    assert_eq!(recovered.source(), projection.source());
    assert_eq!(recovered.records().len(), 1);
    assert_eq!(
        RowReceipt::from_export(recovered.records()[0].row(), recovered.records()[0].row_ordinal()).unwrap(),
        fixture.range.manifest().batches[0].rows[0]
    );
}

#[tokio::test]
async fn one_native_batch_repairs_multiple_rows_once_with_original_one_based_ordinals() {
    let (mut source, mut records) = test_support::object_definition_recovery_only_evidence();
    let mut second = records[0].clone();
    let TelemetryRecord::ObjectDefinition(definition) = &mut second else { panic!() };
    let ObjectDefinitionFact::MssqlCachedDefinition(native) = &mut definition.fact else { panic!() };
    native.context.identity.plan_generation = "7".into();
    *definition = ObjectDefinitionRecord::new(definition.source.clone(), definition.fact.clone()).unwrap();
    records.push(second);
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!() };
    snapshot.records = 2;
    snapshot.sources[0].records = 2;
    let fixture = Fixture::new(source, records).await;
    let capture =
        fixture.read(&[EvidenceFamily::ObjectDefinitions], Some(RecoveryPayloadBudget::default())).await.unwrap();
    assert!(capture.is_read_complete());
    assert_eq!(capture.records().iter().map(|row| row.row_ordinal()).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(fixture.reads(), 1);
    assert_eq!(capture.metrics().recovery_objects, 1);
    for (row, receipt) in capture.records().iter().zip(&fixture.range.manifest().batches[0].rows) {
        assert_eq!(&RowReceipt::from_export(row.row(), row.row_ordinal()).unwrap(), receipt);
    }
}

#[tokio::test]
async fn native_representation_recovery_is_shared_by_scalar_wait_families() {
    let (source, records) = test_support::query_evidence();
    let mut record = records.iter().find(|row| matches!(row, TelemetryRecord::QueryWaits(_))).cloned().unwrap();
    let TelemetryRecord::QueryWaits(wait) = &mut record else { panic!() };
    // AverageWait is an actual scalar Real column. Its shared normalizer maps -0 to +0, while the native receipt
    // still binds the original representation. Nested metric JSON would preserve signed zero and prove no loss.
    wait.average = Some(-0.0);
    wait.total = MeasurementValue::Integer("0".into());
    wait.minimum = Some(MeasurementValue::Integer("0".into()));
    wait.maximum = Some(MeasurementValue::Integer("0".into()));
    let mut original = records
        .into_iter()
        .filter(|row| matches!(row, TelemetryRecord::QueryDefinition(_) | TelemetryRecord::PlanDefinition(_)))
        .collect::<Vec<_>>();
    original.push(record);
    let fixture = Fixture::new(source, original).await;
    let batch =
        fixture.range.manifest().batches.iter().find(|batch| batch.stream == TelemetryStream::QueryWaits).unwrap();
    let average = fixture
        .store
        .catalog
        .get(batch.stream)
        .unwrap()
        .columns()
        .iter()
        .position(|field| field.name == "AverageWait")
        .unwrap();
    assert_eq!(
        fixture.store.rows.lock().unwrap()[&batch.batch_id][0][average].as_f64().unwrap().to_bits(),
        0.0f64.to_bits()
    );
    let projection = fixture.read(&[EvidenceFamily::QueryWaits], None).await.unwrap();
    assert_eq!(projection.gaps().len(), 1);
    assert_eq!(projection.gaps()[0].reason, EvidenceGapReason::NativeRepresentationRequired);
    assert!(projection.records().is_empty());
    assert_eq!(fixture.reads(), 0);
    let recovered = fixture.read(&[EvidenceFamily::QueryWaits], Some(RecoveryPayloadBudget::default())).await.unwrap();
    assert!(recovered.is_read_complete());
    let TelemetryRecord::QueryWaits(wait) = &recovered.records()[0].row().record else { panic!() };
    assert_eq!(wait.average.unwrap().to_bits(), (-0.0f64).to_bits());
    assert_eq!(
        RowReceipt::from_export(recovered.records()[0].row(), recovered.records()[0].row_ordinal()).unwrap(),
        batch.rows[0]
    );
    assert_eq!(recovered.metrics().recovery_objects, 1);
    assert_eq!(recovered.metrics().recovery_bytes, batch.bytes);
    assert_eq!(fixture.reads(), 1);
}

#[tokio::test]
async fn whole_object_bytes_and_object_count_are_preflighted_before_any_raw_open() {
    let fixture = Fixture::objects().await;
    let bytes = fixture.range.manifest().batches[0].bytes;
    let error = fixture
        .read(
            &[EvidenceFamily::ObjectDefinitions],
            Some(RecoveryPayloadBudget { max_objects: 1, max_bytes: bytes - 1 }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert_eq!(fixture.reads(), 0);
    let (source, records) = two_streams();
    let fixture = Fixture::new(source, records).await;
    let error = fixture
        .read(
            &[EvidenceFamily::QueryDefinitions, EvidenceFamily::PlanDefinitions],
            Some(RecoveryPayloadBudget { max_objects: 1, ..Default::default() }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert_eq!(fixture.reads(), 0);
}

#[tokio::test]
async fn missing_proof_publication_rows_and_families_never_authorize_payload_reads() {
    for unavailable in ["publication", "proof", "rows", "family"] {
        let fixture = Fixture::objects().await;
        let families = if unavailable == "family" {
            &[EvidenceFamily::ResourceSamples][..]
        } else {
            &[EvidenceFamily::ObjectDefinitions][..]
        };
        match unavailable {
            "publication" => fixture.store.published.store(false, Ordering::SeqCst),
            "proof" => fixture.proofs.objects.lock().unwrap().clear(),
            "rows" => fixture.store.rows.lock().unwrap().values_mut().for_each(Vec::clear),
            _ => (),
        }
        let capture = fixture.read(families, Some(RecoveryPayloadBudget::default())).await.unwrap();
        assert!(!capture.is_read_complete());
        assert_eq!(fixture.reads(), 0, "{unavailable}");
    }
}

#[tokio::test]
async fn native_unavailable_payloads_remain_native_facts_without_raw_retries() {
    let (source, rows) = test_support::object_definition_evidence_cases().remove(0);
    let fixture = Fixture::new(source, rows).await;
    let capture =
        fixture.read(&[EvidenceFamily::ObjectDefinitions], Some(RecoveryPayloadBudget::default())).await.unwrap();
    assert!(capture.is_read_complete());
    assert_eq!(fixture.reads(), 0);
    assert_eq!(capture.metrics().recovery_bytes, 0);
    assert!(capture.records().iter().any(|row| match &row.row().record {
        TelemetryRecord::ObjectDefinition(row) => match &row.fact {
            ObjectDefinitionFact::MssqlCachedDefinition(definition) => matches!(
                definition.estimated_plan.payload,
                DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
            ),
            _ => false,
        },
        _ => false,
    }));
}

#[tokio::test]
async fn corrupt_truncated_and_late_error_native_tails_do_not_release_early_rows() {
    for fault in [Fault::Corrupt, Fault::Truncate, Fault::LateError] {
        let fixture = Fixture::objects().await;
        *fixture.recovery.fault.lock().unwrap() = fault;
        let mut capture = gaps(&fixture.range);
        let before = capture.gaps.clone();
        assert!(
            restore(&fixture.range, &fixture.recovery, RecoveryPayloadBudget::default(), &mut capture).await.is_err()
        );
        assert!(capture.records.is_empty());
        assert_eq!(capture.gaps, before);
    }
}

#[tokio::test]
async fn wrong_native_receipt_in_a_later_object_leaves_every_repair_uninstalled() {
    let catalog = PhysicalCatalog::<JsonProjection>::new().unwrap();
    let (source, records) = two_streams();
    let mut recovery = Recovery::capture("fixture:hydration", catalog.digest(), source, records, 201_002);
    let selected = recovery.manifest.batches.iter_mut().filter(|batch| !batch.rows.is_empty()).collect::<Vec<_>>();
    assert_eq!(selected.len(), 2);
    let later = selected.into_iter().max_by_key(|batch| batch.batch_id.clone()).unwrap();
    later.rows[0].payload_digest = digest(b"foreign-native-record");
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let mut capture = gaps(&range);
    let before = capture.gaps.clone();
    let error = restore(&range, &recovery, RecoveryPayloadBudget::default(), &mut capture).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    assert_eq!(recovery.object_reads.load(Ordering::SeqCst), 2);
    assert!(
        capture.metrics.recovery_bytes > 0,
        "an earlier object reached verified EOF before the later receipt failed"
    );
    assert!(capture.records.is_empty());
    assert_eq!(capture.gaps, before);
}

#[tokio::test]
async fn absent_authorized_store_is_rejected_before_warehouse_or_payload_work() {
    let fixture = Fixture::objects().await;
    let error = fixture
        .reader
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::ObjectDefinitions],
            EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..Default::default() },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::InvalidContract)));
    assert_eq!(fixture.store.queries.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.reads(), 0);
}

#[tokio::test(start_paused = true)]
async fn public_reader_deadline_covers_the_payload_eof_wait_without_exposing_rows() {
    let fixture = Fixture::objects().await;
    *fixture.recovery.fault.lock().unwrap() = Fault::Stall;
    let error = fixture
        .reader
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::ObjectDefinitions],
            EvidenceReadBudget {
                recovery_payloads: Some(RecoveryPayloadBudget::default()),
                timeout_ms: 1,
                ..Default::default()
            },
            Some(&fixture.recovery),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut)));
    assert_eq!(fixture.reads(), 1);
}

#[tokio::test(start_paused = true)]
async fn always_ready_native_body_yields_to_the_same_whole_read_deadline() {
    let fixture = Fixture::objects().await;
    let (result, _) = tokio::join!(
        biased;
        fixture.reader.read_capture(
            &fixture.range,
            &[EvidenceFamily::ObjectDefinitions],
            EvidenceReadBudget {
                recovery_payloads: Some(RecoveryPayloadBudget::default()),
                timeout_ms: 1,
                ..Default::default()
            },
            Some(&fixture.recovery),
        ),
        async {
            while fixture.reads() == 0 {
                tokio::task::yield_now().await;
            }
            tokio::time::advance(std::time::Duration::from_millis(2)).await;
        },
    );
    assert_eq!(result.unwrap_err().detail, Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut)));
    assert_eq!(fixture.reads(), 1);
}
