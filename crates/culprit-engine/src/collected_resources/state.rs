//! One admitted point capture owns its native source coverage and read qualification.
use super::*;

pub(super) struct CapturedState<'a> {
    capture: &'a EvidenceCapture,
    pub native: &'a StateSnapshotDescription,
    pub coverage: &'a StateSourceCoverage,
}
impl<'a> CapturedState<'a> {
    pub fn new(capture: &'a EvidenceCapture, dataset: SnapshotDataset) -> Result<Self> {
        let SourceObservation::Snapshot(native) = &capture.source().observation else {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        };
        if native.dataset != dataset || !capture.families().contains(&EvidenceFamily::OperationalSamples) {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        let [coverage] = native.sources.as_slice() else { return Err(invalid()) };
        if !capture.source().target.is_valid() || !native.is_valid() {
            return Err(invalid());
        }
        Ok(Self { capture, native, coverage })
    }

    pub fn shared(&self, kinds: &[SharedResourceKind]) -> Result<&'a SharedResourceIdentity> {
        let CollectionTarget::Shared(resource) = &self.capture.source().target else {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        };
        if !kinds.contains(&resource.kind) {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        Ok(resource)
    }

    pub fn database(&self) -> Result<&'a culprit_core::DatabaseIdentity> {
        self.capture.source().target.database().ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())
    }

    pub fn bounds(&self) -> Result<ObservationBounds> {
        ObservationBounds::new(&self.native.observed_from, &self.native.observed_until).ok_or_else(invalid)
    }

    pub fn rows(&self) -> Result<Vec<&'a OperationalObservation>> {
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

    pub fn outcome(&self, rows: usize) -> Result<SourceOutcome> {
        if rows > self.coverage.records as usize {
            return Err(invalid());
        }
        let incomplete_read = !self.capture.is_read_complete() || rows != self.coverage.records as usize;
        Ok(SourceOutcome {
            source: self.coverage.source.clone(),
            state: if incomplete_read { SourceState::Partial } else { self.coverage.state },
            reason: incomplete_read.then(|| culprit_core::t!("engine.collected_resources.incomplete_read")),
            reason_message: incomplete_read
                .then(|| culprit_core::message!("engine.collected_resources.incomplete_read")),
        })
    }
}
