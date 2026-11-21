use super::*;
use culprit_collector::outbox::Outbox;
use culprit_core::{ErrorDetail, telemetry::*};
use std::time::Duration;

fn attempt(id: &str, digest: &str) -> DiagnosticAttemptReceipt {
    DiagnosticAttemptReceipt {
        capture: CaptureIdentity {
            observation_id: id.into(),
            revision: Some(CaptureRevision {
                outbox_id: "test-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        },
        manifest_sha256: Some(digest.repeat(64)),
    }
}
fn input<'a>(
    delivery: &'a OutboxDelivery,
    attempt: &'a DiagnosticAttemptReceipt,
    cancellation: &'a CollectionCancellation,
) -> CompletedCaptureScanInput<'a> {
    CompletedCaptureScanInput {
        delivery,
        attempt,
        cancellation,
        analysis: CompletedCaptureAnalysis::ProcessCpu,
        budget: CompletedCaptureBudget::default(),
    }
}
#[test]
fn all_conflicts_are_refused_before_the_selected_original_count_limit() {
    let home = tempfile::tempdir().unwrap();
    let outbox = Outbox::create(&home.path().join("outbox"), Default::default()).unwrap();
    let delivery = outbox.delivery();
    let first = attempt("first", "a");
    let conflict = attempt("first", "b");
    let cancellation = CollectionCancellation::default();
    let mut inputs =
        (0..=MAX_RECORDED_OPERATIONAL_READS).map(|_| input(&delivery, &first, &cancellation)).collect::<Vec<_>>();
    inputs.push(input(&delivery, &conflict, &cancellation));
    assert_eq!(validate_selection(&inputs).unwrap_err().detail, Some(ErrorDetail::Delivery(DeliveryProblem::Conflict)));
    inputs.pop();
    assert_eq!(validate_selection(&inputs).unwrap_err().detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
}
#[test]
fn exact_repeated_claims_remain_valid_but_unknown_attestation_and_cancellation_do_not() {
    let home = tempfile::tempdir().unwrap();
    let outbox = Outbox::create(&home.path().join("outbox"), Default::default()).unwrap();
    let delivery = outbox.delivery();
    let first = attempt("first", "a");
    let mut absent = first.clone();
    absent.manifest_sha256 = None;
    let cancellation = CollectionCancellation::default();
    assert!(
        validate_selection(&[input(&delivery, &first, &cancellation), input(&delivery, &first, &cancellation)]).is_ok()
    );
    // Analysis choice is not capture identity. Each real reader still authenticates its selected native family.
    let mut other_analysis = input(&delivery, &first, &cancellation);
    other_analysis.analysis = CompletedCaptureAnalysis::MemoryClerks;
    assert!(validate_selection(&[input(&delivery, &first, &cancellation), other_analysis]).is_ok());
    assert!(validate_selection(&[input(&delivery, &absent, &cancellation)]).is_err());
    cancellation.cancel();
    assert_eq!(
        validate_selection(&[input(&delivery, &first, &cancellation)]).unwrap_err().detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled))
    );
}
#[test]
fn exact_shared_row_byte_boundary_is_paid_once_and_cannot_be_renewed_for_another_original() {
    let budget = CompletedCaptureBudget { max_rows: 7, max_bytes: 100, timeout: Duration::from_secs(5) };
    let mut allowance = OriginalReadAllowance::new(budget, Instant::now()).unwrap();
    let original =
        CompletedOriginal { evidence: super::super::CapturedScanEvidence::Multiple(vec![]), rows: 7, bytes: 100 };
    allowance.charge(&original).unwrap();
    assert_eq!((allowance.rows, allowance.bytes), (0, 0));
    assert!(allowance.charge(&original).is_err());
}
#[test]
fn each_original_only_tightens_the_one_action_allowance_and_deadline() {
    let home = tempfile::tempdir().unwrap();
    let outbox = Outbox::create(&home.path().join("outbox"), Default::default()).unwrap();
    let delivery = outbox.delivery();
    let first = attempt("first", "a");
    let cancellation = CollectionCancellation::default();
    let mut selected = input(&delivery, &first, &cancellation);
    selected.budget = CompletedCaptureBudget { max_rows: 2, max_bytes: 4, timeout: Duration::from_secs(1) };
    let mut allowance = OriginalReadAllowance::new(
        CompletedCaptureBudget { max_rows: 7, max_bytes: 100, timeout: Duration::from_secs(5) },
        Instant::now(),
    )
    .unwrap();
    let (narrowed, cutoff) = allowance.for_input(&selected).unwrap();
    assert!(cutoff <= allowance.deadline);
    assert_eq!((narrowed.max_rows, narrowed.max_bytes), (2, 4));
    assert!(narrowed.timeout <= Duration::from_secs(1));
    allowance.deadline = Instant::now();
    assert_eq!(
        allowance.for_input(&selected).unwrap_err().detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut))
    );
}

#[test]
fn actual_plural_and_single_saved_scan_futures_keep_original_vectors_and_reports_on_the_heap() {
    fn future_size<A, F: std::future::Future>(_: impl FnOnce(A) -> F) -> usize {
        std::mem::size_of::<F>()
    }
    type Args = (&'static Workspace, &'static str, &'static SnapshotOptions, CompletedCaptureScanInput<'static>);
    assert!(
        future_size(|(workspace, name, options, input): Args| workspace.scan_populations_with_completed_capture(
            name,
            options,
            input,
            culprit_core::ProviderOperationObservation::unobserved()
        )) <= 4096
    );
    type Plural = (
        &'static Workspace,
        &'static str,
        &'static SnapshotOptions,
        Vec<CompletedCaptureScanInput<'static>>,
        CompletedCaptureBudget,
    );
    assert!(
        future_size(|(workspace, name, options, inputs, budget): Plural| workspace
            .scan_populations_with_completed_captures(
                name,
                options,
                inputs,
                budget,
                culprit_core::ProviderOperationObservation::unobserved()
            ))
            <= 4096
    );
    assert!(std::mem::size_of::<CompletedOriginal>() <= 4096);
}

#[test]
fn original_local_deadline_includes_selection_preflight_before_any_original_worker() {
    let started = Instant::now() - Duration::from_secs(2);
    let error = OriginalReadAllowance::new(
        CompletedCaptureBudget { timeout: Duration::from_secs(1), ..Default::default() },
        started,
    )
    .err()
    .unwrap();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut)));
}
