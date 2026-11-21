//! Original retained table rows enter the same catalog explanation as a scan, without a current database join.
use super::*;
use culprit_core::{TableTemporalReport, collected_resource_analysis::CollectedTableTemporalAnalysis};

pub fn table_temporal(capture: &EvidenceCapture) -> Result<CollectedTableTemporalAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::Tables)?;
    let rows = input.rows()?;
    let report = explain(capture.source(), input.coverage, &rows, capture.is_read_complete())?;
    Ok(CollectedTableTemporalAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}

pub(crate) fn from_read(read: &CapturedOperationalRead) -> Result<TableTemporalReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    let native = read.description.observation.snapshot().ok_or_else(invalid)?;
    let [coverage] = native.sources.as_slice() else { return Err(invalid()) };
    explain(&read.description, coverage, &read.observations.iter().collect::<Vec<_>>(), read.read_complete)
}

fn explain(
    description: &CollectionDescription,
    coverage: &StateSourceCoverage,
    observations: &[&OperationalObservation],
    read_complete: bool,
) -> Result<TableTemporalReport> {
    let source = read_outcome(coverage, observations.len(), read_complete)?;
    culprit_checks::table_temporal::explain(culprit_checks::table_catalog::TableCatalogEvidence {
        description,
        observations,
        source: &source,
        read_complete,
    })
    .map_err(|_| invalid())
}
