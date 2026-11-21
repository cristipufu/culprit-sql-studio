//! Original resource intervals use the same canonical interpreter as local and saved reads.
use super::*;
use culprit_core::collected_resource_analysis::CollectedManagedInstanceResourceAnalysis;
pub fn managed_instance_resources(capture: &EvidenceCapture) -> Result<CollectedManagedInstanceResourceAnalysis> {
    super::references::validate_capture(capture)?;
    let SourceObservation::MssqlManagedInstanceResources(history) = &capture.source().observation else {
        return Err(invalid());
    };
    if capture.captured_rows(TelemetryStream::ResourceSamples) != Some(history.coverage.records) {
        return Err(invalid());
    }
    let mut selected = capture
        .records()
        .iter()
        .filter(|record| record.row().record.stream() == TelemetryStream::ResourceSamples)
        .collect::<Vec<_>>();
    selected.sort_by_key(|record| record.row_ordinal());
    let mut positions = Vec::new();
    let mut observations = Vec::new();
    for record in selected {
        let reference = super::references::from_record(capture, record)?;
        let TelemetryRecord::ResourceSample(row) = &record.row().record else { return Err(invalid()) };
        reference.validate_canonical_record(capture.source(), &record.row().record)?;
        positions.push(CapturedSnapshotPosition::from_export(record.row(), reference.row_ordinal)?);
        observations.push(row.clone());
    }
    let read_complete =
        !capture.gaps().iter().any(|gap| gap.stream.is_none_or(|stream| stream == TelemetryStream::ResourceSamples));
    let read = CapturedManagedInstanceResourceRead::captured(
        capture.source().clone(),
        DiagnosticCaptureRef {
            capture: capture.capture().clone(),
            manifest_sha256: capture.capture_manifest_sha256().into(),
            source_scope_id: capture.source().captured_range()?.scope_id,
        },
        positions,
        observations,
        read_complete,
    )?;
    let report = culprit_checks::managed_instance_resources::explain_captured(&read)?;
    Ok(CollectedManagedInstanceResourceAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
