use super::*;
use crate::{
    analytical::{
        ProjectionCatalog,
        catalog::PhysicalCatalog,
        evidence::{EvidenceFamily, EvidenceGapReason, EvidenceReadBudget, EvidenceReader},
        indexes::ProjectionIndexes,
        sink::AnalyticalSink,
        test_support::{self, JsonProjection},
    },
    capture::{CollectionCancellation, SourceFactory, collect},
    delivery::{
        contract::Contract,
        file_store::FileRecoveryStore,
        recovery::{CommittedRange, RecoveryStore, stage_and_commit},
        sink::{DeliveryAttempt, Submission, TelemetrySink, VerificationBudget, verify},
    },
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
mod warehouse;
use warehouse::Warehouse;

struct NativeFixture {
    source: CollectionDescription,
    records: Vec<TelemetryRecord>,
    closed: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl SourceFactory for NativeFixture {
    async fn open(&self, _: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        Ok(Box::new(Self { source: self.source.clone(), records: self.records.clone(), closed: self.closed.clone() }))
    }
}
#[async_trait::async_trait]
impl TelemetrySource for NativeFixture {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        Ok(self.source.clone())
    }
    fn query_count(&self) -> u32 {
        1
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        assert!(cursor.is_none());
        Ok(CollectionRead::Page {
            records: self.records.iter().filter(|row| row.stream() == stream).cloned().collect(),
            next_cursor: None,
        })
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        Ok(SourceCompletion::Verified)
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed.store(true, Ordering::SeqCst);
        SourceShutdown::Quiescent
    }
}

fn request(source: &CollectionDescription) -> CollectionRequest {
    let history = source.query_history().unwrap();
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::QueryHistory {
            from: history.interval.from.clone(),
            until: history.interval.until.clone(),
        },
        budget: CollectionBudget::default(),
    }
}
async fn collect_into(
    lease: &mut CaptureLease<'_>,
    source: CollectionDescription,
    records: Vec<TelemetryRecord>,
) -> CollectionManifest {
    let request = request(&source);
    let closed = Arc::new(AtomicBool::new(false));
    let factory = NativeFixture { source, records, closed: closed.clone() };
    let directory = lease.directory().to_owned();
    let identity = lease.identity().clone();
    let manifest = collect(
        &culprit_read::admission::SourceGate::default(),
        &factory,
        request,
        &directory,
        identity,
        &CollectionCancellation::default(),
        Some(lease),
    )
    .await
    .unwrap();
    assert!(closed.load(Ordering::SeqCst), "native closure precedes every durable readiness decision");
    manifest
}
async fn capture(outbox: &mut Outbox, records: Vec<TelemetryRecord>) -> CollectionManifest {
    let (source, _) = test_support::informational_query_evidence();
    let mut lease = outbox.reserve("fixture", &request(&source)).await.unwrap();
    let manifest = collect_into(&mut lease, source, records).await;
    lease.finish().await.unwrap();
    manifest
}

#[tokio::test]
async fn unresolved_rows_survive_real_capture_restart_commit_readback_and_committed_retirement() {
    let root = tempfile::tempdir().unwrap();
    let catalog = Arc::new(PhysicalCatalog::<JsonProjection>::new().unwrap());
    let contract =
        Contract::new("pipeline".into(), "shard".into(), "fixture:informational".into(), catalog.digest().into())
            .unwrap();
    let store = Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap());
    let owner = store.acquire().await.unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create_remote(&path, OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    outbox.begin_remote_epoch(owner.fence(), 0).await.unwrap();
    let (source, records) = test_support::informational_query_evidence();
    let manifest = capture(&mut outbox, records.clone()).await;
    assert_eq!(manifest.status, CollectionStatus::Complete);
    assert!(manifest.references.integrity_complete);
    assert!(!manifest.references.complete);
    assert_eq!(manifest.references.queries.unresolved, 3);
    assert_eq!(manifest.references.plans.unresolved, 1);
    let sealed =
        std::fs::read(path.join("captures").join(&manifest.capture.observation_id).join("manifest.json")).unwrap();
    let charged = outbox.status(20, None).await.unwrap().charged_bytes;
    assert!(charged > 0);
    assert!(outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    drop(outbox);
    let mut outbox = Outbox::open(&path).unwrap();
    outbox.recover().await.unwrap();
    assert_eq!(outbox.status(20, None).await.unwrap().charged_bytes, charged);
    let native_range = source.captured_range().unwrap();
    assert_eq!(outbox.captured_range(&native_range).await.unwrap(), Some(manifest.capture.clone()));
    let delivery = outbox.delivery();
    let prepared = delivery.prepare(&manifest.capture.observation_id, contract.clone(), vec![]).await.unwrap();
    assert_eq!(prepared.manifest().capture_manifest_sha256, digest(&sealed));
    assert_eq!(prepared.manifest().reference_coverage, manifest.references);
    let committed = stage_and_commit(store.as_ref(), owner.fence(), None, &prepared).await.unwrap();
    let original = store.load(&committed.manifest().range_id).await.unwrap();
    assert_eq!(original, *prepared.manifest());
    assert_eq!(store.captured_range(&native_range).await.unwrap(), Some(manifest.capture.clone()));
    let receipts = original.expected_receipts().collect::<Vec<_>>();
    assert_eq!(receipts.iter().filter(|row| row.record_id.is_none()).count(), 3);
    for batch in &original.batches {
        for (index, row) in batch.rows.iter().enumerate() {
            assert_eq!(row.key.row_ordinal as usize, index + 1);
        }
    }
    let indexes = Arc::new(ProjectionIndexes::new(Arc::new(test_support::Blobs::default())));
    let warehouse = Arc::new(Warehouse::new(catalog));
    let sink =
        AnalyticalSink::new(contract, store.clone(), indexes.clone(), warehouse.clone(), &root.path().join("scratch"))
            .unwrap();
    let attempt = DeliveryAttempt::new(1).unwrap();
    for batch in &original.batches {
        if batch.delivery_required {
            sink.dispatch(&committed, &attempt, &batch.batch_id).await.unwrap();
        }
    }
    let verified = verify(&sink, &committed, VerificationBudget { page_rows: 1, ..Default::default() }).await.unwrap();
    let published = sink.publish(&verified, &attempt).await.unwrap();
    assert!(sink.publication_visible(&published, &committed).await.unwrap());
    let reader = EvidenceReader::new(warehouse.clone(), indexes);
    let families = [EvidenceFamily::QueryRuntime, EvidenceFamily::QueryWaits];
    let captured = reader
        .read_capture(&committed, &families, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
        .await
        .unwrap();
    assert!(captured.is_read_complete(), "{:?}", captured.gaps());
    assert_eq!(captured.source(), &source);
    assert_eq!(captured.reference_coverage(), &manifest.references);
    assert_eq!(captured.captured_rows(TelemetryStream::QueryRuntime), Some(3));
    assert_eq!(captured.captured_rows(TelemetryStream::QueryDefinitions), None);
    assert_eq!(captured.metrics().recovery_objects, 0);
    for actual in captured.records() {
        let expected =
            records.iter().find(|row| row.native_identity() == actual.row().record.native_identity()).unwrap();
        let exported = ExportedRecord::new(&source, &manifest.capture, expected.clone()).unwrap();
        assert_eq!(actual.row(), &exported);
        assert_eq!(actual.native_payload_sha256(), exported.payload_sha256().unwrap());
        if exported.record_id.is_none() {
            assert!(
                DiagnosticEvidenceRef::from_export(&source, &digest(&sealed), &exported, actual.row_ordinal() - 1)
                    .is_err()
            );
        }
        if let TelemetryRecord::QueryRuntime(row) = &actual.row().record {
            assert_eq!(row.executions, "9007199254740993");
        }
    }
    let runtime = original.batches.iter().find(|batch| batch.stream == TelemetryStream::QueryRuntime).unwrap();
    warehouse.omit(&runtime.batch_id, 0);
    let partial = reader
        .read_capture(&committed, &families, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
        .await
        .unwrap();
    assert_eq!(partial.captured_rows(TelemetryStream::QueryRuntime), Some(3));
    assert!(partial.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(partial.metrics().recovery_objects, 0);
    assert!(delivery.evict(&committed).await.unwrap());
    assert_eq!(outbox.status(20, None).await.unwrap().charged_bytes, 0);
    assert_eq!(store.captured_range(&native_range).await.unwrap(), Some(manifest.capture));
}

#[tokio::test]
async fn an_unresolved_neighbor_cannot_hide_a_dangling_declared_native_definition() {
    let root = tempfile::tempdir().unwrap();
    let (_, mut outbox) = box_at(&root);
    let (_, mut records) = test_support::informational_query_evidence();
    records.retain(|row| !matches!(row, TelemetryRecord::QueryDefinition(_)));
    let manifest = capture(&mut outbox, records).await;
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::MissingDefinitions));
    assert!(!manifest.references.integrity_complete);
    assert!(
        outbox
            .captured_range(&manifest.description.as_ref().unwrap().captured_range().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(outbox.status(20, None).await.unwrap().entries[0].state, OutboxCaptureState::Incomplete);
    assert!(
        outbox
            .delivery()
            .prepare(
                &manifest.capture.observation_id,
                Contract::new("pipeline".into(), "shard".into(), "fixture:informational".into(), digest(b"schema"))
                    .unwrap(),
                vec![]
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn original_artifact_admission_rejects_duplicate_unidentified_rows_forged_ids_and_foreign_revisions() {
    for change in ["duplicate", "semantic-id", "foreign-revision", "semantic-coverage"] {
        let root = tempfile::tempdir().unwrap();
        let (_, mut outbox) = box_at(&root);
        let (source, records) = test_support::informational_query_evidence();
        let mut lease = outbox.reserve("fixture", &request(&source)).await.unwrap();
        let mut manifest = collect_into(&mut lease, source, records).await;
        let file = manifest.files.iter_mut().find(|file| file.stream == TelemetryStream::QueryRuntime).unwrap();
        let path = lease.directory().join(&file.file);
        let bytes = std::fs::read(&path).unwrap();
        let mut lines = std::str::from_utf8(&bytes).unwrap().lines().map(str::to_owned).collect::<Vec<_>>();
        let mut row: ExportedRecord = serde_json::from_str(&lines[0]).unwrap();
        assert!(row.record_id.is_none());
        match change {
            "duplicate" => {
                lines.push(lines[0].clone());
                file.rows += 1;
            }
            "semantic-id" => {
                row.record_id = Some(digest(b"fabricated-semantic-identity"));
                lines[0] = serde_json::to_string(&row).unwrap();
            }
            "foreign-revision" => {
                row.capture.revision.as_mut().unwrap().owner_epoch = "2".into();
                lines[0] = serde_json::to_string(&row).unwrap();
            }
            _ => {
                manifest.references.queries.unresolved = 0;
                manifest.references.queries.required = manifest.references.queries.resolved;
                manifest.references.plans.unresolved = 0;
                manifest.references.plans.required = manifest.references.plans.resolved;
                manifest.references.complete = true;
            }
        }
        let changed = format!("{}\n", lines.join("\n")).into_bytes();
        std::fs::write(&path, &changed).unwrap();
        file.bytes = changed.len().try_into().unwrap();
        file.sha256 = Some(digest(&changed));
        std::fs::write(lease.directory().join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        let error = lease.finish().await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Outbox(OutboxProblem::Integrity)), "{change}");
        assert_eq!(outbox.status(20, None).await.unwrap().entries[0].state, OutboxCaptureState::Quarantined);
    }
}
