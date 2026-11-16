use super::*;
use std::sync::atomic::Ordering;
use test_support::{PageWriterCase, page_writer_evidence};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn page_writer_preview_withholds_relationships_until_the_exact_selected_native_body_is_hydrated() {
    for case in PageWriterCase::ALL {
        let (source, records) = page_writer_evidence(case);
        let fixture = make_fixture(source.clone(), records.clone()).await;
        let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let physical = native_rows(&fixture);
        let public = serde_json::to_string(&physical).unwrap();
        assert!(!public.contains("private-writer-machine-canary"));
        assert!(!public.contains("private-writer-instance-canary"));
        fixture.replies(vec![physical.clone()]);
        let preview = read(&fixture, false).await.unwrap();
        assert_eq!(preview.source(), &source);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        if !has_plan(&records) {
            test_support::assert_operational_capture(&preview, &source, &records);
            assert_eq!(preview.metrics().recovery_objects, 0);
            continue;
        }
        assert!(!preview.is_read_complete(), "{case:?}");
        assert!(preview.records().is_empty(), "a native verdict cannot survive without its selected body");
        assert_eq!(preview.gaps().len(), 1);
        assert!(matches!(
            preview.gaps()[0].reason,
            EvidenceGapReason::DetailInRecovery | EvidenceGapReason::NativeRepresentationRequired
        ));
        *fixture.recovery.fault.lock().unwrap() = Fault::None;
        fixture.replies(vec![physical]);
        let capture = read(&fixture, true).await.unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().recovery_objects, 1);
        assert_eq!(capture.metrics().recovery_bytes, fixture.range.manifest().batches[0].bytes);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst) - opens, 1);
        assert_eq!(capture.capture_manifest_sha256(), fixture.range.manifest().capture_manifest_sha256);
        assert_eq!(capture.recovery_manifest_sha256(), fixture.range.manifest_digest());
    }
}

#[tokio::test]
async fn recovery_cannot_fill_a_missing_writer_row_or_upgrade_the_original_native_scope() {
    let (source, records) = page_writer_evidence(PageWriterCase::TargetWriter);
    let fixture = make_fixture(source.clone(), records).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    fixture.replies(vec![vec![]]);
    let capture = read(&fixture, true).await.unwrap();
    assert!(capture.records().is_empty());
    assert!(!capture.is_read_complete());
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::MissingRows);
    assert_eq!(capture.source(), &source);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
}

#[tokio::test]
async fn changed_writer_generation_parent_proof_or_focus_cannot_borrow_the_original_receipt() {
    let (source, records) = page_writer_evidence(PageWriterCase::TargetWriter);
    let fixture = make_fixture(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|field| field.name == "Details").unwrap();
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    for (pointer, value) in [
        ("/candidates/0/request/key/request/generation", json!("recycled-current-request")),
        ("/candidates/0/parent/manifest_sha256", json!(digest(b"another original native parent"))),
        ("/candidates/0/binding/native_plan_sha256", json!(digest(b"another native plan body"))),
        ("/focus_key", json!(digest(b"another admitted page focus"))),
        ("/page/target/file/qualifiers/create_lsn", json!("a-new-file-epoch")),
    ] {
        let mut rows = native_rows(&fixture);
        let mut details: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).unwrap() = value;
        rows[0][column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let error = read(&fixture, true).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    }
}

#[tokio::test]
async fn writer_native_tail_corruption_or_late_read_failure_never_releases_a_partial_relationship() {
    let (source, records) = page_writer_evidence(PageWriterCase::TargetWriter);
    let fixture = make_fixture(source, records).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    for fault in [Fault::Corrupt, Fault::Truncate, Fault::LateError] {
        *fixture.recovery.fault.lock().unwrap() = fault;
        fixture.replies(vec![native_rows(&fixture)]);
        assert!(read(&fixture, true).await.is_err());
    }
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst) - opens, 3);
}

fn has_plan(records: &[TelemetryRecord]) -> bool {
    records.iter().any(|record| {
        matches!(record, TelemetryRecord::Operational(row)
        if matches!(&row.fact, OperationalFact::PageWaitWriters(writers)
            if writers.candidates.iter().any(|candidate| candidate.plan.is_available())))
    })
}

async fn make_fixture(source: CollectionDescription, records: Vec<TelemetryRecord>) -> Fixture {
    Fixture::new(source, records).await
}
fn native_rows(fixture: &Fixture) -> Vec<Vec<Value>> {
    fixture.rows_for(EvidenceFamily::OperationalSamples)
}
async fn read(fixture: &Fixture, recovery: bool) -> culprit_core::Result<EvidenceCapture> {
    fixture
        .reader()
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget { recovery_payloads: recovery.then(RecoveryPayloadBudget::default), ..budget() },
            recovery.then_some(fixture.recovery.as_ref() as &dyn culprit_collector::delivery::recovery::RecoveryStore),
        )
        .await
}
