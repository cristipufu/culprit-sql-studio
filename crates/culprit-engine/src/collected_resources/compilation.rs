//! Exact database compilation history remains independent of runtime intervals and current compiler admission.
use super::*;
use culprit_checks::compilation::{CompilationDefinitionEvidence, CompilationDefinitionRecord, CompilationEvidence};
use culprit_core::collected_resource_analysis::CollectedCompilationAnalysis;

pub fn compilation(
    capture: &EvidenceCapture,
    baseline: Option<&EvidenceCapture>,
    definitions: Option<&EvidenceCapture>,
) -> Result<CollectedCompilationAnalysis> {
    if baseline.is_some_and(|baseline| baseline.range_id() == capture.range_id())
        || definitions.is_some_and(|definitions| {
            definitions.range_id() == capture.range_id()
                || baseline.is_some_and(|baseline| baseline.range_id() == definitions.range_id())
        })
    {
        return Err(DeliveryProblem::InvalidContract.error());
    }
    let input = CapturedState::new(capture, SnapshotDataset::QueryCompilation)?;
    if input.database()?.engine != Engine::Mssql {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    input.bounds()?;
    let rows = input.rows()?;
    let source = input.outcome(rows.len())?;
    let context = NativeReadContext::from_snapshot(capture.source())?;
    let definition_rows = definitions.map(definition_rows).transpose()?;
    let definition_evidence =
        definitions.zip(definition_rows.as_deref()).map(|(capture, rows)| CompilationDefinitionEvidence {
            source: capture.source(),
            rows,
            selected_rows: capture.metrics().selected_rows,
            read_complete: capture.is_read_complete(),
        });
    let evidence = CompilationEvidence {
        context: Some(&context),
        observations: &rows,
        source: Some(&source),
        definitions: definition_evidence,
    };
    let mut report = culprit_checks::compilation::explain(evidence).map_err(|_| invalid())?;
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
        report.comparison = Some(culprit_checks::compilation::compare(evidence, &previous).map_err(|_| invalid())?);
    }
    Ok(CollectedCompilationAnalysis {
        context: crate::collected_context::from_capture(capture),
        baseline_context: baseline.map(crate::collected_context::from_capture),
        definition_context: definitions.map(|capture| Box::new(crate::collected_context::from_capture(capture))),
        report,
    })
}

fn definition_rows(capture: &EvidenceCapture) -> Result<Vec<CompilationDefinitionRecord<'_>>> {
    references::validate_capture(capture)?;
    if capture.families() != [EvidenceFamily::QueryDefinitions]
        || capture.source().query_history().is_none()
        || capture.source().target.engine() != Engine::Mssql
    {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    capture
        .records()
        .iter()
        .map(|record| {
            let TelemetryRecord::QueryDefinition(definition) = &record.row().record else { return Err(invalid()) };
            Ok(CompilationDefinitionRecord { reference: references::from_record(capture, record)?, definition })
        })
        .collect()
}
