//! Local original compilation history uses the same pure view, without fabricating an archive capture.
use super::*;
use culprit_checks::compilation::CompilationEvidence;
use culprit_collector::outbox::CompletedSnapshot;
use culprit_core::CompilationReport;

/// The canonical local read remains available once, independently of its derived explanation. This value is
/// deliberately not serializable: it cannot masquerade as an archive EvidenceCapture or a freshly acquired read.
#[derive(Debug)]
pub struct CompletedCompilationAnalysis {
    original: CompletedSnapshot,
    report: Box<CompilationReport>,
}
impl CompletedCompilationAnalysis {
    pub fn original(&self) -> &CompletedSnapshot {
        &self.original
    }
    pub fn report(&self) -> &CompilationReport {
        &self.report
    }
}

/// Explain only original, fully verified local bytes. Definition/body hydration and native collection are
/// separate explicit actions; the physical execution header never replaces Query Store's Target data context.
pub fn completed_compilation(original: CompletedSnapshot) -> Result<CompletedCompilationAnalysis> {
    let source = original.source();
    let native = source.observation.snapshot().ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    if native.dataset != SnapshotDataset::QueryCompilation {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    if !source.partition_is_consistent() || !source.observation.covers_request() {
        return Err(invalid());
    }
    let [coverage] = native.sources.as_slice() else { return Err(invalid()) };
    let rows = original
        .records()
        .iter()
        .map(|record| match &record.record {
            TelemetryRecord::Operational(row) if row.source == coverage.source => Ok(row),
            _ => Err(invalid()),
        })
        .collect::<Result<Vec<_>>>()?;
    let outcome = state::read_outcome(coverage, rows.len(), original.is_read_complete())?;
    let context = NativeReadContext::from_snapshot(source)?;
    let report = culprit_checks::compilation::explain(CompilationEvidence {
        context: Some(&context),
        observations: &rows,
        source: Some(&outcome),
        definitions: None,
    })?;
    Ok(CompletedCompilationAnalysis { original, report: Box::new(report) })
}

#[cfg(test)]
mod tests;
