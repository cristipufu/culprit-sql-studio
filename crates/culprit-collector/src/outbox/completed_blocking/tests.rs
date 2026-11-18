use super::super::completed_capture::ReadControl;
use super::*;
use crate::analytical::test_support::{BlockingCase, LocalCaptureFixture, blocking_evidence, blocking_evidence_cases};
use culprit_core::{ErrorDetail, limits::OperationGuard};
use std::path::PathBuf;

async fn read(fixture: &LocalCaptureFixture, budget: CompletedCaptureBudget) -> Result<CompletedBlocking> {
    let guard = OperationGuard::new(budget.timeout).unwrap();
    fixture
        .outbox
        .as_ref()
        .unwrap()
        .delivery()
        .with_completed_blocking(
            CompletedCaptureLookup {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
            },
            budget,
            guard.handle(),
            (),
            Ok,
        )
        .await
}

fn parent(fixture: &LocalCaptureFixture) -> DiagnosticCaptureRef {
    let SourceObservation::FocusedBlocking { result, .. } = &fixture.manifest.description.as_ref().unwrap().observation
    else {
        panic!()
    };
    result.focus.parent.capture_ref()
}
fn parent_directory(fixture: &LocalCaptureFixture) -> PathBuf {
    fixture.capture_directory.parent().unwrap().join(parent(fixture).capture.observation_id)
}
fn parent_manifest(fixture: &LocalCaptureFixture) -> CollectionManifest {
    serde_json::from_slice(&std::fs::read(parent_directory(fixture).join("manifest.json")).unwrap()).unwrap()
}
fn original_bytes(fixture: &LocalCaptureFixture) -> u64 {
    let directory = parent_directory(fixture);
    let manifest = parent_manifest(fixture);
    std::fs::metadata(directory.join("manifest.json")).unwrap().len()
        + manifest.files.iter().map(|file| u64::from(file.bytes)).sum::<u64>()
}

#[tokio::test]
async fn a_focused_capture_loads_its_exact_original_once_without_replacing_either_native_scope_or_clock() {
    for (source, rows) in blocking_evidence_cases() {
        let fixture = LocalCaptureFixture::activity(source, rows).await;
        let pair = read(&fixture, CompletedCaptureBudget::default()).await.unwrap();
        assert_eq!(pair.original().capture(), &parent(&fixture));
        assert_eq!(&pair.followup().capture().capture, &fixture.manifest.capture);
        assert_eq!(pair.original().connection(), "local-evidence");
        assert_eq!(pair.followup().connection(), pair.original().connection());
        assert_eq!(pair.original().source(), parent_manifest(&fixture).description.as_ref().unwrap());
        assert_eq!(pair.followup().source(), fixture.manifest.description.as_ref().unwrap());
        assert_eq!(pair.original().records().len(), 1);
        for input in [pair.original(), pair.followup()] {
            let frame = input.frame().unwrap();
            assert!(frame.read_complete());
            assert_eq!(frame.rows().count(), frame.native().observations as usize);
            for reference in input.references() {
                assert_eq!(&reference.capture_ref(), input.capture());
            }
        }
        assert_ne!(pair.original().frame().unwrap().native().at, pair.followup().frame().unwrap().native().at);
        assert_eq!(pair.original().bytes_read(), original_bytes(&fixture));
        assert_eq!(pair.followup().bytes_read(), fixture.original_bytes());
        assert_eq!(pair.bytes_read(), original_bytes(&fixture) + fixture.original_bytes());
        assert_eq!(pair.followup().collected_from(), fixture.manifest.started_at);
        assert_eq!(Some(pair.followup().collected_until()), fixture.manifest.finished_at.as_deref());
    }
}

#[tokio::test]
async fn both_originals_share_one_row_byte_and_absolute_deadline_allowance() {
    let (source, rows) = blocking_evidence(BlockingCase::Matched);
    let fixture = LocalCaptureFixture::activity(source, rows).await;
    let bytes = original_bytes(&fixture) + fixture.original_bytes();
    let rows = fixture.manifest.description.as_ref().unwrap().observation.activity().unwrap().observations + 1;
    assert!(rows > 2);
    for budget in [
        CompletedCaptureBudget { max_rows: rows - 1, ..Default::default() },
        CompletedCaptureBudget { max_bytes: bytes - 1, ..Default::default() },
    ] {
        assert_eq!(
            read(&fixture, budget).await.unwrap_err().detail,
            Some(ErrorDetail::Delivery(DeliveryProblem::Capacity))
        );
    }
    let pair = read(&fixture, CompletedCaptureBudget { max_rows: rows, max_bytes: bytes, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(pair.bytes_read(), bytes);

    let budget = CompletedCaptureBudget::default();
    let guard = OperationGuard::new(budget.timeout).unwrap();
    let mut read = CompletedCaptureRead::activity_lookup(
        CompletedCaptureLookup {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
        },
        budget,
        guard.handle(),
    )
    .unwrap();
    let ReadControl::Operation { deadline: before, .. } = &read.control else { panic!() };
    let before = *before;
    read.select_activity_reference("local-evidence", parent(&fixture)).unwrap();
    let ReadControl::Operation { deadline: after, .. } = &read.control else { panic!() };
    assert_eq!(before, *after, "selecting a prerequisite never restarts the owning deadline");
    drop(guard);
    assert_eq!(
        read.select_activity_reference("local-evidence", parent(&fixture)).unwrap_err().detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled))
    );
}

#[tokio::test]
async fn a_cold_missing_or_changed_original_cannot_be_substituted_by_a_valid_followup() {
    for remove in [true, false] {
        let (source, rows) = blocking_evidence(BlockingCase::Matched);
        let mut fixture = LocalCaptureFixture::activity(source, rows).await;
        let original = parent_directory(&fixture);
        if remove {
            std::fs::remove_file(original.join("manifest.json")).unwrap();
        } else {
            let manifest = parent_manifest(&fixture);
            let body = original.join(&manifest.files[0].file);
            let text = std::fs::read_to_string(&body).unwrap();
            let forged = text.replace("native-pg-statement-42", "native-pg-statement-99");
            assert_ne!(text, forged);
            assert_eq!(text.len(), forged.len());
            std::fs::write(body, forged).unwrap();
        }
        drop(fixture.outbox.take());
        fixture.outbox = Some(super::super::Outbox::open(&fixture.directory).unwrap());
        let error = read(&fixture, CompletedCaptureBudget::default()).await.unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::Integrity)));
    }
}

#[tokio::test]
async fn exact_capture_binding_refuses_other_authenticated_generations_and_connections() {
    let (source, rows) = blocking_evidence(BlockingCase::Matched);
    let fixture = LocalCaptureFixture::activity(source, rows).await;
    let original = read(&fixture, CompletedCaptureBudget::default()).await.unwrap();
    let (mut source, rows) = blocking_evidence(BlockingCase::Matched);
    let SourceObservation::FocusedBlocking { result, .. } = &mut source.observation else { panic!() };
    result.focus.original.key.session.generation = "2020-01-01T10:59:42Z".into();
    // This is a separately captured original with a valid new generation, not a rewritten sealed body.
    result.focus.parent.native_key = NativeRecordIdentity::Activity(result.focus.original.key.clone());
    // The follow-up must also carry that same original key to remain an internally coherent closure.
    let mut rows = rows;
    for row in &mut rows {
        if let TelemetryRecord::Activity(row) = row
            && row.key.session.id == "42"
        {
            row.key = result.focus.original.key.clone();
        }
    }
    let second = LocalCaptureFixture::activity(source, rows).await;
    let second = read(&second, CompletedCaptureBudget::default()).await.unwrap();
    let (foreign, _) = second.into_inputs();
    let (_, followup) = original.into_inputs();
    assert_eq!(
        CompletedBlocking::new(foreign, followup, 0).unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::Integrity))
    );

    let guard = OperationGuard::new(CompletedCaptureBudget::default().timeout).unwrap();
    let error = fixture
        .outbox
        .as_ref()
        .unwrap()
        .delivery()
        .with_completed_blocking(
            CompletedCaptureLookup {
                connection: "different-connection",
                observation_id: &fixture.manifest.capture.observation_id,
            },
            CompletedCaptureBudget::default(),
            guard.handle(),
            (),
            Ok,
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture)));
}

#[tokio::test]
async fn an_unsettled_followup_never_projects_a_successful_empty_closure() {
    let (source, rows) = blocking_evidence(BlockingCase::Matched);
    let fixture = LocalCaptureFixture::with_shutdown(source, rows, SourceShutdown::Unconfirmed).await;
    let error = read(&fixture, CompletedCaptureBudget::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::SourceUnconfirmed)));
}
