//! Tuning points explain independent native state, without attaching a neighboring workload or live definition.
use super::*;
use culprit_core::collected_resource_analysis::CollectedTuningAnalysis;

pub fn tuning(capture: &EvidenceCapture) -> Result<CollectedTuningAnalysis> {
    if matches!(capture.source().partition, SourcePartition::InstanceLocal { .. }) {
        let original = NativeReadContext::from_snapshot(capture.source())?;
        if original.target.engine() != Engine::Mssql
            || !original.has_qualified_local_owner()
            || !matches!(original.partition,SourcePartition::InstanceLocal{member:Some(ref member),..} if member.kind==SharedResourceKind::Instance)
        {
            return Err(invalid());
        }
    }
    let inputs = CapturedState::sources_for(capture, SnapshotDataset::Tuning, EvidenceFamily::TuningObservations)?;
    let mut observations = Vec::new();
    let mut sources = Vec::new();
    for input in inputs {
        input.database()?;
        input.bounds()?;
        let rows = input.tuning()?;
        sources.push(input.outcome(rows.len())?);
        observations.extend(rows);
    }
    let report = culprit_checks::tuning::explain(&observations, &sources).map_err(|_| invalid())?;
    Ok(CollectedTuningAnalysis { context: crate::collected_context::from_capture(capture), report })
}
