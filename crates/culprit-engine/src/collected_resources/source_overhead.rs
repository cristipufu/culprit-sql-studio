//! Source-read overhead is already in the authenticated capture manifest; native records need not be reread.
use culprit_collector::analytical::evidence::EvidenceCaptureMetadata;
use culprit_core::{CollectedSourceOverheadAnalysis, Result, SourceOverheadEvidence, t, telemetry::CollectionStatus};

pub fn source_overhead(metadata: &EvidenceCaptureMetadata) -> Result<CollectedSourceOverheadAnalysis> {
    let evidence = SourceOverheadEvidence {
        capture: metadata.capture().clone(),
        engine: Some(metadata.source().target.engine()),
        source: Some(metadata.source().clone()),
        status: CollectionStatus::Complete,
        started_at: metadata.collected_from().into(),
        finished_at: Some(metadata.collected_until().into()),
        source_queries: Some(metadata.source_queries()),
        read_metrics: metadata.source_read_metrics().cloned(),
    };
    let mut report = culprit_checks::source_overhead::explain(&evidence)?;
    report.limitations.push(t!("view.source_overhead.limit.archive"));
    Ok(CollectedSourceOverheadAnalysis {
        range_id: metadata.range_id().into(),
        recovery_manifest_sha256: metadata.recovery_manifest_sha256().into(),
        capture_manifest_sha256: metadata.capture_manifest_sha256().into(),
        read_gaps: metadata.gaps().to_vec(),
        metrics: crate::collected_context::read_metrics(metadata.metrics()),
        report,
    })
}
