//! Authenticated operational snapshots can be explained without a provider, credentials or an invented database.
use culprit_collector::analytical::evidence::{EvidenceCapture, EvidenceFamily};
use culprit_core::{
    Engine, Result, SourceOutcome, SourceState, collected_resource_analysis::CollectedMemoryFailureAnalysis,
    operations::MemoryFailureSnapshot, telemetry::*,
};

/// Explain this one captured retained buffer. Positions identify rows within the capture, not OOM incidents
/// across captures. The source's offset-free event clock remains distinct from UTC capture and delivery clocks.
pub fn memory_failures(capture: &EvidenceCapture) -> Result<CollectedMemoryFailureAnalysis> {
    let (resource, native) = qualified_snapshot(capture)?;
    let [native_source] = native.sources.as_slice() else {
        return Err(invalid());
    };
    let mut observations = Vec::new();
    for record in capture.records() {
        let TelemetryRecord::Operational(observation) = &record.row().record else {
            return Err(invalid());
        };
        let OperationalFact::MssqlOutOfMemory(failure) = &observation.fact else {
            return Err(invalid());
        };
        if observation.source != native_source.source || !observation.is_valid() {
            return Err(invalid());
        }
        observations.push(failure.as_ref().clone());
    }
    // Physical batch order is not the native capture order. This sorts the captured positions, never event time.
    observations.sort_by_key(|observation| observation.position);
    if observations.len() > native_source.records as usize {
        return Err(invalid());
    }
    let incomplete_read = !capture.is_read_complete() || observations.len() != native_source.records as usize;
    let source = SourceOutcome {
        source: native_source.source.clone(),
        state: if incomplete_read { SourceState::Partial } else { native_source.state },
        reason: incomplete_read.then(|| culprit_core::t!("engine.collected_memory_failures.incomplete_read")),
        reason_message: incomplete_read
            .then(|| culprit_core::message!("engine.collected_memory_failures.incomplete_read")),
    };
    let report = {
        let evidence = MemoryFailureSnapshot {
            source: native_source.source.clone(),
            observed_from: native.observed_from.clone(),
            observed_until: native.observed_until.clone(),
            scope: capture.source().target.measurement_scope(),
            resource: Some(resource.clone()),
            observations,
        };
        culprit_checks::memory_failures::explain(&evidence, Some(&source))?
    };
    Ok(CollectedMemoryFailureAnalysis { context: crate::collected_context::from_capture(capture), report })
}

fn qualified_snapshot(capture: &EvidenceCapture) -> Result<(&SharedResourceIdentity, &StateSnapshotDescription)> {
    let CollectionTarget::Shared(resource) = &capture.source().target else {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    };
    let SourceObservation::Snapshot(snapshot) = &capture.source().observation else {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    };
    if resource.engine != Engine::Mssql
        || !matches!(resource.kind, SharedResourceKind::Instance | SharedResourceKind::Pool)
        || snapshot.dataset != SnapshotDataset::MemoryFailures
        || !capture.families().contains(&EvidenceFamily::OperationalSamples)
    {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    if !resource.is_valid() || !snapshot.is_valid() {
        return Err(invalid());
    }
    Ok((resource, snapshot))
}

fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
