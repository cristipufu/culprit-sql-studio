//! Exact database compilation history remains independent of runtime intervals and current compiler admission.
use super::*;
use culprit_core::collected_resource_analysis::CollectedCompilationAnalysis;

pub fn compilation(
    capture: &EvidenceCapture,
    baseline: Option<&EvidenceCapture>,
) -> Result<CollectedCompilationAnalysis> {
    if baseline.is_some_and(|baseline| baseline.range_id() == capture.range_id()) {
        return Err(DeliveryProblem::InvalidContract.error());
    }
    let input = CapturedState::new(capture, SnapshotDataset::QueryCompilation)?;
    if input.database()?.engine != Engine::Mssql {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    input.bounds()?;
    let rows = input.rows()?;
    let source = input.outcome(rows.len())?;
    let mut report = culprit_checks::compilation::explain(&rows, Some(&source)).map_err(|_| invalid())?;
    if let Some(baseline) = baseline {
        let before = CapturedState::new(baseline, SnapshotDataset::QueryCompilation)?;
        if before.database()?.engine != Engine::Mssql {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        before.bounds()?;
        let before_rows = before.rows()?;
        let before_source = before.outcome(before_rows.len())?;
        let previous = CompilationBaseline::from_observations(
            &before.coverage.source,
            Some(NativeReadContext::from_snapshot(baseline.source())?),
            Some(before_source.state),
            &before_rows,
            None,
        )?;
        let context = NativeReadContext::from_snapshot(capture.source())?;
        report.comparison = Some(
            culprit_checks::compilation::compare(Some(&context), &rows, Some(&source), &previous)
                .map_err(|_| invalid())?,
        );
    }
    Ok(CollectedCompilationAnalysis {
        context: crate::collected_context::from_capture(capture),
        baseline_context: baseline.map(crate::collected_context::from_capture),
        report,
    })
}
