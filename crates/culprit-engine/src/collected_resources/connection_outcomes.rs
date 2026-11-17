//! Archived native counters use the scan's pure explanation; analysis never opens a source or recovery object.
use super::*;
use culprit_core::{ConnectionOutcomeSnapshot, collected_resource_analysis::CollectedConnectionOutcomeAnalysis};

pub fn connection_outcomes(capture: &EvidenceCapture) -> Result<CollectedConnectionOutcomeAnalysis> {
    let inputs = CapturedState::sources(capture, SnapshotDataset::Operational)?;
    let mut rows = Vec::new();
    for input in inputs {
        for row in input.rows()? {
            if !matches!(row.fact, OperationalFact::CounterSet(_)) {
                return Err(DeliveryProblem::UnsupportedSchema.error());
            }
            rows.push(row.clone());
        }
    }
    let native = ConnectionOutcomeSnapshot::from_source(capture.source(), rows).map_err(|_| invalid())?;
    let report =
        culprit_checks::connection_outcomes::explain(&native, capture.is_read_complete()).map_err(|_| invalid())?;
    Ok(CollectedConnectionOutcomeAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
