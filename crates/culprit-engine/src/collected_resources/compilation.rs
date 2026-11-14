//! Exact database compilation history remains independent of runtime intervals and current compiler admission.
use super::*;
use culprit_core::collected_resource_analysis::CollectedCompilationAnalysis;

pub fn compilation(capture: &EvidenceCapture) -> Result<CollectedCompilationAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::QueryCompilation)?;
    if input.database()?.engine != Engine::Mssql {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    input.bounds()?;
    let rows = input.rows()?;
    let source = input.outcome(rows.len())?;
    let report = culprit_checks::compilation::explain(&rows, Some(&source)).map_err(|_| invalid())?;
    Ok(CollectedCompilationAnalysis { context: crate::collected_context::from_capture(capture), report })
}
