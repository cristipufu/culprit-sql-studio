//! Optional native questions wait for absent sealed evidence; they never repair corrupt evidence or acknowledge it.
use crate::{
    capture::CollectionCancellation, delivery::recovery::RecoveryStore, diagnostics::resolve_evidence, outbox::Outbox,
};
use culprit_core::{Error, ErrorDetail, Result, telemetry::*};

pub(super) async fn resolve_optional_evidence(
    outbox: &mut Outbox,
    connection: &str,
    reference: &DiagnosticEvidenceRef,
    recovery: Option<&dyn RecoveryStore>,
    cancellation: &CollectionCancellation,
) -> Result<Option<(CollectionDescription, ExportedRecord)>> {
    match resolve_evidence(outbox, connection, reference, recovery, cancellation).await {
        Err(error) if evidence_unavailable(&error) => Ok(None),
        evidence => evidence,
    }
}

pub(super) fn evidence_unavailable(error: &Error) -> bool {
    matches!(
        error.detail,
        Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture))
            | Some(ErrorDetail::Delivery(DeliveryProblem::NotReady))
    )
}
