//! The metadata route neither requires complete native rows nor opens result streams.
use super::*;
use culprit_collector::outbox::InspectedCaptureManifest;
use culprit_core::{
    CapturedSourceAcquisition, HistoricalSourceOverhead, RecordedSourceAcquisition, SourceAcquisitionSeal,
    SourceOverheadEvidence, message,
};

impl OutboxCaptureReader {
    pub(super) fn metadata_projection(
        &self,
        connection: String,
    ) -> impl FnOnce(Option<InspectedCaptureManifest>) -> Result<LocalCapturedAnalysis> + Send + 'static {
        #[cfg(test)]
        let boundary = self.projection_boundary.clone();
        move |capture| {
            let result =
                explain(connection, capture).map(|analysis| LocalCapturedAnalysis::SourceOverhead(Box::new(analysis)));
            #[cfg(test)]
            if let Some(boundary) = boundary {
                boundary()?;
            }
            result
        }
    }
}
fn explain(connection: String, capture: Option<InspectedCaptureManifest>) -> Result<LocalSourceOverhead> {
    let Some(capture) = capture else {
        let reason = message!("view.source_overhead.initial_missing");
        return Ok(LocalSourceOverhead {
            connection,
            bytes_read: 0,
            original: RecordedSourceAcquisition::Unavailable { reason: reason.clone() },
            report: HistoricalSourceOverhead::Unavailable { reason: reason.render() },
        });
    };
    if capture.connection() != connection {
        return Err(DeliveryProblem::Integrity.error());
    }
    let evidence = SourceOverheadEvidence::from_manifest(capture.manifest());
    // Validate raw authority/metrics once before retaining only the curated, display-only allowlist.
    culprit_checks::source_overhead::explain(&evidence)?;
    let seal = if capture.is_sealed() { SourceAcquisitionSeal::Sealed } else { SourceAcquisitionSeal::Provisional };
    let original = Box::new(CapturedSourceAcquisition::captured(capture.sha256().into(), seal, evidence.summary()?)?);
    let report = Box::new(culprit_checks::source_overhead::explain_recorded(&original)?);
    Ok(LocalSourceOverhead {
        connection,
        bytes_read: capture.bytes_read(),
        report: HistoricalSourceOverhead::Available { manifest_sha256: original.manifest_sha256.clone(), report },
        original: RecordedSourceAcquisition::Captured { original },
    })
}
