//! Authenticated resource observations need neither a live provider nor invented query ownership.
use culprit_collector::analytical::evidence::{EvidenceCapture, EvidenceFamily};
use culprit_core::{
    Engine, Result, SourceOutcome, SourceState, collected_resource_analysis::CollectedRetainedFailureAnalysis,
    operations::MemoryFailureSnapshot, telemetry::*,
};
mod state;
use state::CapturedState;
mod request_memory;
pub use request_memory::request_memory;
mod session_memory;
pub use session_memory::session_memory;
mod page_waits;
pub use page_waits::page_waits;

/// Positions identify captured rows, never incidents across observations. Native offset-free event time remains
/// distinct from UTC capture and delivery clocks.
pub fn memory_failures(capture: &EvidenceCapture) -> Result<CollectedRetainedFailureAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::MemoryFailures)?;
    let resource = input.shared(&[SharedResourceKind::Instance, SharedResourceKind::Pool])?;
    if resource.engine != Engine::Mssql {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let mut observations = input
        .rows()?
        .into_iter()
        .map(|observation| {
            let OperationalFact::MssqlOutOfMemory(failure) = &observation.fact else { return Err(invalid()) };
            Ok(failure.as_ref().clone())
        })
        .collect::<Result<Vec<_>>>()?;
    observations.sort_by_key(|observation| observation.position);
    let source = input.outcome(observations.len())?;
    let report = {
        let evidence = MemoryFailureSnapshot {
            source: input.coverage.source.clone(),
            observed_from: input.native.observed_from.clone(),
            observed_until: input.native.observed_until.clone(),
            scope: capture.source().target.measurement_scope(),
            resource: Some(resource.clone()),
            observations,
        };
        culprit_checks::memory_failures::explain(&evidence, Some(&source))?
    };
    Ok(CollectedRetainedFailureAnalysis { context: crate::collected_context::from_capture(capture), report })
}

/// Only exact declared operational rows from this authenticated shared capture are admitted. Current sessions and
/// neighboring captures are never consulted to fill missing query, resource-owner or native-clock information.
pub fn native_failures(capture: &EvidenceCapture) -> Result<CollectedRetainedFailureAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::NativeFailures)?;
    let resource = input.shared(&[SharedResourceKind::Instance])?;
    let mut observations = input
        .rows()?
        .into_iter()
        .map(|observation| match &observation.fact {
            OperationalFact::MysqlErrorLog(row) => Ok(NativeFailureRecord::MysqlErrorLog(row.as_ref().clone())),
            OperationalFact::PostgresLog(row) => Ok(NativeFailureRecord::PostgresLog(row.clone())),
            _ => Err(invalid()),
        })
        .collect::<Result<Vec<_>>>()?;
    observations.sort_by_key(NativeFailureRecord::position);
    let source = input.outcome(observations.len())?;
    let report = {
        let evidence = NativeFailureSnapshot {
            source: input.coverage.source.clone(),
            observed_from: input.native.observed_from.clone(),
            observed_until: input.native.observed_until.clone(),
            resource: Some(resource.clone()),
            observations,
        };
        culprit_checks::native_failures::explain(&evidence, Some(&source))?
    };
    Ok(CollectedRetainedFailureAnalysis { context: crate::collected_context::from_capture(capture), report })
}

fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
