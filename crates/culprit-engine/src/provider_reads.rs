//! Pure current-operation projection. Source/Outbox history stays a separate input and display.
use culprit_core::{
    ObservedProbeAttempt, OriginalProbeResult, ProbeOutcome, ProviderAcquisitionReport, ProviderAcquisitionSummary,
    ProviderAttempt, ProviderReadDisposition, ServerInfo,
};

pub fn explain_provider_acquisition(summary: &ProviderAcquisitionSummary) -> ProviderAcquisitionReport {
    culprit_checks::provider_acquisition::explain(summary)
}
pub fn project_probe_attempt(attempt: ProviderAttempt<ServerInfo>) -> ObservedProbeAttempt {
    let (original, finalization, summary) = attempt.into_parts();
    let acquisition = explain_provider_acquisition(&summary);
    let original = match original {
        Some(Ok(server)) => OriginalProbeResult::Connected { server },
        Some(Err(error)) => OriginalProbeResult::Failed { error },
        None => OriginalProbeResult::NotMaterialized,
    };
    let outcome = if summary.native_disposition == ProviderReadDisposition::Unconfirmed {
        ProbeOutcome::NativeWorkUnconfirmed {
            original,
            error: finalization
                .err()
                .unwrap_or_else(|| culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error()),
        }
    } else if let Err(error) = finalization {
        ProbeOutcome::FinalizationFailed { original, error }
    } else {
        match original {
            OriginalProbeResult::Connected { server } => ProbeOutcome::Connected { server },
            OriginalProbeResult::Failed { error } => ProbeOutcome::Failed { error },
            OriginalProbeResult::NotMaterialized => unreachable!("a missing output requires failed finalization"),
        }
    };
    ObservedProbeAttempt { outcome, acquisition }
}

pub(crate) fn requested_call(
    call: culprit_core::ProviderCall<crate::PopulationReports>,
) -> culprit_core::ProviderCall<Box<culprit_core::Report>> {
    call.map(|reports| reports.requested)
}

pub fn project_scan_attempt(attempt: ProviderAttempt<Box<culprit_core::Report>>) -> culprit_core::ObservedScanAttempt {
    let (original, finalization, summary) = attempt.into_parts();
    let acquisition = explain_provider_acquisition(&summary);
    if summary.native_disposition == ProviderReadDisposition::Unconfirmed {
        return culprit_core::ObservedScanAttempt::NativeWorkUnconfirmed {
            original_failure: original.and_then(|result| result.err()),
            error: finalization
                .err()
                .unwrap_or_else(|| culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error()),
            acquisition,
        };
    }
    if let Err(error) = finalization {
        return culprit_core::ObservedScanAttempt::FinalizationFailed {
            original_failure: original.and_then(|result| result.err()),
            error,
            acquisition,
        };
    }
    match original {
        Some(Ok(report)) => culprit_core::ObservedScanAttempt::Complete { report },
        Some(Err(error)) => culprit_core::ObservedScanAttempt::Failed { error, acquisition },
        None => unreachable!("a missing output requires failed finalization"),
    }
}

#[cfg(all(test, feature = "collector"))]
mod tests;
