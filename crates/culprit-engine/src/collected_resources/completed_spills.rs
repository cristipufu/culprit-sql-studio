//! Optional definitions share no execution population with the original counters; every receipt stays separate.
use super::*;
use culprit_checks::completed_spills::{CompletedSpillEvidence, CompletedSpillRecord};
use culprit_core::collected_resource_analysis::CollectedCompletedSpillAnalysis;

pub fn completed_spills(
    capture: &EvidenceCapture,
    definitions: Option<&EvidenceCapture>,
) -> Result<CollectedCompletedSpillAnalysis> {
    if definitions.is_some_and(|definitions| definitions.range_id() == capture.range_id()) {
        return Err(DeliveryProblem::InvalidContract.error());
    }
    let runtime = CapturedSpills::new(capture, SnapshotDataset::CachedRuntime, EvidenceFamily::OperationalSamples)?;
    let definitions = definitions
        .map(|capture| {
            CapturedSpills::new(
                capture,
                capture
                    .source()
                    .observation
                    .snapshot()
                    .filter(|snapshot| {
                        matches!(snapshot.dataset, SnapshotDataset::CachedDefinitions | SnapshotDataset::RetainedPlans)
                    })
                    .ok_or_else(invalid)?
                    .dataset,
                EvidenceFamily::ObjectDefinitions,
            )
        })
        .transpose()?;
    let report = culprit_checks::completed_spills::explain(
        runtime.evidence(),
        definitions.as_ref().map(CapturedSpills::evidence),
    )?;
    Ok(CollectedCompletedSpillAnalysis {
        context: crate::collected_context::from_capture(capture),
        definition_context: definitions.as_ref().map(|input| crate::collected_context::from_capture(input.capture)),
        report,
    })
}

struct CapturedSpills<'a> {
    capture: &'a EvidenceCapture,
    rows: Vec<CompletedSpillRecord<'a>>,
    sources: Vec<SourceOutcome>,
}
impl<'a> CapturedSpills<'a> {
    fn new(capture: &'a EvidenceCapture, dataset: SnapshotDataset, family: EvidenceFamily) -> Result<Self> {
        references::validate_capture(capture)?;
        let inputs = CapturedState::sources_for(capture, dataset, family)?;
        let mut sources = Vec::with_capacity(inputs.len());
        for input in inputs {
            input.database()?;
            input.bounds()?;
            let count = match family {
                EvidenceFamily::OperationalSamples => input.rows()?.len(),
                EvidenceFamily::ObjectDefinitions => input.definitions()?.len(),
                _ => return Err(invalid()),
            };
            sources.push(input.outcome(count)?);
        }
        let rows = capture
            .records()
            .iter()
            .map(|record| {
                Ok(CompletedSpillRecord {
                    reference: references::from_record(capture, record)?,
                    record: &record.row().record,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { capture, rows, sources })
    }
    fn evidence(&self) -> CompletedSpillEvidence<'_> {
        CompletedSpillEvidence {
            source: self.capture.source(),
            rows: &self.rows,
            sources: &self.sources,
            read_complete: self.capture.is_read_complete(),
        }
    }
}
