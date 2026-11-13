//! Authenticated retained diagnostics need neither a provider nor an invented database or query owner.
use culprit_collector::analytical::evidence::{EvidenceCapture, EvidenceFamily};
use culprit_core::{
    Engine, Result, SourceOutcome, SourceState, collected_resource_analysis::CollectedRetainedFailureAnalysis,
    operations::MemoryFailureSnapshot, telemetry::*,
};

/// Positions identify captured rows, never incidents across observations. Native offset-free event time remains
/// distinct from UTC capture and delivery clocks.
pub fn memory_failures(capture: &EvidenceCapture) -> Result<CollectedRetainedFailureAnalysis> {
    let input = CapturedState::new(
        capture,
        SnapshotDataset::MemoryFailures,
        &[SharedResourceKind::Instance, SharedResourceKind::Pool],
    )?;
    if input.resource.engine != Engine::Mssql {
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
            resource: Some(input.resource.clone()),
            observations,
        };
        culprit_checks::memory_failures::explain(&evidence, Some(&source))?
    };
    Ok(CollectedRetainedFailureAnalysis { context: crate::collected_context::from_capture(capture), report })
}

/// Only exact declared operational rows from this authenticated shared capture are admitted. Current sessions and
/// neighboring captures are never consulted to fill missing query, resource-owner or native-clock information.
pub fn native_failures(capture: &EvidenceCapture) -> Result<CollectedRetainedFailureAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::NativeFailures, &[SharedResourceKind::Instance])?;
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
            resource: Some(input.resource.clone()),
            observations,
        };
        culprit_checks::native_failures::explain(&evidence, Some(&source))?
    };
    Ok(CollectedRetainedFailureAnalysis { context: crate::collected_context::from_capture(capture), report })
}

struct CapturedState<'a> {
    capture: &'a EvidenceCapture,
    resource: &'a SharedResourceIdentity,
    native: &'a StateSnapshotDescription,
    coverage: &'a StateSourceCoverage,
}
impl<'a> CapturedState<'a> {
    fn new(capture: &'a EvidenceCapture, dataset: SnapshotDataset, kinds: &[SharedResourceKind]) -> Result<Self> {
        let CollectionTarget::Shared(resource) = &capture.source().target else {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        };
        let SourceObservation::Snapshot(native) = &capture.source().observation else {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        };
        if native.dataset != dataset
            || !kinds.contains(&resource.kind)
            || !capture.families().contains(&EvidenceFamily::OperationalSamples)
        {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        let [coverage] = native.sources.as_slice() else { return Err(invalid()) };
        if !resource.is_valid() || !native.is_valid() {
            return Err(invalid());
        }
        Ok(Self { capture, resource, native, coverage })
    }

    fn rows(&self) -> Result<Vec<&'a OperationalObservation>> {
        self.capture
            .records()
            .iter()
            .map(|record| {
                let TelemetryRecord::Operational(observation) = &record.row().record else {
                    return Err(invalid());
                };
                if observation.source != self.coverage.source || !observation.is_valid() {
                    return Err(invalid());
                }
                Ok(observation)
            })
            .collect()
    }

    fn outcome(&self, rows: usize) -> Result<SourceOutcome> {
        if rows > self.coverage.records as usize {
            return Err(invalid());
        }
        let incomplete_read = !self.capture.is_read_complete() || rows != self.coverage.records as usize;
        Ok(SourceOutcome {
            source: self.coverage.source.clone(),
            state: if incomplete_read { SourceState::Partial } else { self.coverage.state },
            reason: incomplete_read.then(|| culprit_core::t!("engine.collected_failures.incomplete_read")),
            reason_message: incomplete_read
                .then(|| culprit_core::message!("engine.collected_failures.incomplete_read")),
        })
    }
}

fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
