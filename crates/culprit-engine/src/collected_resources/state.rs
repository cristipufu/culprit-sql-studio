//! One admitted point capture owns its native source coverage and read qualification.
use super::*;

pub(super) struct CapturedState<'a> {
    capture: &'a EvidenceCapture,
    family: EvidenceFamily,
    pub native: &'a StateSnapshotDescription,
    pub coverage: &'a StateSourceCoverage,
}
impl<'a> CapturedState<'a> {
    pub fn new(capture: &'a EvidenceCapture, dataset: SnapshotDataset) -> Result<Self> {
        let family = EvidenceFamily::OperationalSamples;
        let native = native_state(capture, dataset, family)?;
        let [coverage] = native.sources.as_slice() else { return Err(invalid()) };
        Ok(Self { capture, family, native, coverage })
    }

    /// Independent native sources retain their individual coverage within one authenticated capture.
    pub fn sources(capture: &'a EvidenceCapture, dataset: SnapshotDataset) -> Result<Vec<Self>> {
        Self::sources_for(capture, dataset, EvidenceFamily::OperationalSamples)
    }

    pub fn sources_for(
        capture: &'a EvidenceCapture,
        dataset: SnapshotDataset,
        family: EvidenceFamily,
    ) -> Result<Vec<Self>> {
        let native = native_state(capture, dataset, family)?;
        Ok(native.sources.iter().map(|coverage| Self { capture, family, native, coverage }).collect())
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
        if self.family != EvidenceFamily::OperationalSamples {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        self.capture
            .records()
            .iter()
            .filter_map(|record| {
                let TelemetryRecord::Operational(observation) = &record.row().record else {
                    return Some(Err(invalid()));
                };
                if observation.source != self.coverage.source {
                    return None;
                }
                Some(Ok(observation))
            })
            .collect()
    }

    /// A focused aggregate keeps its one native row or its independent read gap, never a selected prefix.
    pub fn single_row(&self) -> Result<Option<&'a OperationalObservation>> {
        let rows = self.rows()?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => Ok(Some(*row)),
            _ => Err(invalid()),
        }
    }

    pub fn definitions(&self) -> Result<Vec<&'a ObjectDefinitionRecord>> {
        if self.family != EvidenceFamily::ObjectDefinitions {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        self.capture
            .records()
            .iter()
            .filter_map(|record| {
                let TelemetryRecord::ObjectDefinition(definition) = &record.row().record else {
                    return Some(Err(invalid()));
                };
                (definition.source == self.coverage.source).then_some(Ok(definition))
            })
            .collect()
    }

    pub fn replication(&self) -> Result<Vec<&'a ReplicationObservation>> {
        if self.family != EvidenceFamily::ReplicationSamples {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        self.capture
            .records()
            .iter()
            .filter_map(|record| {
                let TelemetryRecord::Replication(observation) = &record.row().record else {
                    return Some(Err(invalid()));
                };
                (observation.source == self.coverage.source).then_some(Ok(observation))
            })
            .collect()
    }

    pub fn wait_counters(&self) -> Result<Vec<&'a WaitCounterObservation>> {
        if self.family != EvidenceFamily::WaitCounterSamples {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        self.capture
            .records()
            .iter()
            .filter_map(|record| {
                let TelemetryRecord::WaitCounter(row) = &record.row().record else {
                    return Some(Err(invalid()));
                };
                (row.source == self.coverage.source).then_some(Ok(row))
            })
            .collect()
    }

    pub fn tuning(&self) -> Result<Vec<&'a TuningObservation>> {
        if self.family != EvidenceFamily::TuningObservations {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        self.capture
            .records()
            .iter()
            .filter_map(|record| {
                let TelemetryRecord::Tuning(observation) = &record.row().record else {
                    return Some(Err(invalid()));
                };
                (observation.source == self.coverage.source).then_some(Ok(observation))
            })
            .collect()
    }

    pub fn outcome(&self, rows: usize) -> Result<SourceOutcome> {
        read_outcome(self.coverage, rows, self.capture.is_read_complete())
    }
}

/// One read qualification policy for authenticated archive selections and original local snapshots.
pub(super) fn read_outcome(coverage: &StateSourceCoverage, rows: usize, read_complete: bool) -> Result<SourceOutcome> {
    if rows > coverage.records as usize {
        return Err(invalid());
    }
    // A warehouse gap cannot turn an explicitly unavailable native source into a partially read population.
    let readable = match coverage.state {
        SourceState::Collected | SourceState::Empty | SourceState::Partial => true,
        SourceState::Unavailable | SourceState::NotApplicable => false,
    };
    let incomplete_read = readable && (!read_complete || rows != coverage.records as usize);
    Ok(SourceOutcome {
        source: coverage.source.clone(),
        state: if incomplete_read { SourceState::Partial } else { coverage.state },
        reason: incomplete_read.then(|| culprit_core::t!("engine.collected_resources.incomplete_read")),
        reason_message: incomplete_read.then(|| culprit_core::message!("engine.collected_resources.incomplete_read")),
    })
}

fn native_state(
    capture: &EvidenceCapture,
    dataset: SnapshotDataset,
    family: EvidenceFamily,
) -> Result<&StateSnapshotDescription> {
    let native = capture.source().observation.snapshot().ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    if native.dataset != dataset || dataset.stream() != family.stream() || !capture.families().contains(&family) {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    if !capture.source().target.is_valid() || !native.is_valid() {
        return Err(invalid());
    }
    for record in capture.records() {
        let source = match (&record.row().record, family) {
            (TelemetryRecord::Operational(observation), EvidenceFamily::OperationalSamples)
                if observation.is_valid() =>
            {
                &observation.source
            }
            (TelemetryRecord::ObjectDefinition(definition), EvidenceFamily::ObjectDefinitions)
                if definition.is_valid() =>
            {
                &definition.source
            }
            (TelemetryRecord::Tuning(observation), EvidenceFamily::TuningObservations) if observation.is_valid() => {
                &observation.source
            }
            (TelemetryRecord::Replication(observation), EvidenceFamily::ReplicationSamples)
                if observation.is_valid() =>
            {
                &observation.source
            }
            (TelemetryRecord::WaitCounter(row), EvidenceFamily::WaitCounterSamples)
                if row.is_valid_for(capture.source().target.engine()) =>
            {
                &row.source
            }
            _ => return Err(invalid()),
        };
        if !native.sources.iter().any(|coverage| &coverage.source == source) {
            return Err(invalid());
        }
    }
    Ok(native)
}
