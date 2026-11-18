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
    let runtime_rows = runtime.rows()?;
    let definition_rows = definitions.as_ref().map(CapturedSpills::rows).transpose()?;
    let report = culprit_checks::completed_spills::explain(
        runtime.evidence(&runtime_rows),
        definitions.as_ref().zip(definition_rows.as_ref()).map(|(input, rows)| input.evidence(rows)),
    )?;
    Ok(CollectedCompletedSpillAnalysis {
        context: crate::collected_context::from_capture(capture),
        definition_context: definitions.as_ref().map(|input| crate::collected_context::from_capture(input.capture)),
        report,
    })
}

struct CapturedSpills<'a> {
    capture: &'a EvidenceCapture,
    read_id: String,
    runtime: Option<CachedRuntimeRead>,
    sources: Vec<SourceOutcome>,
}
impl<'a> CapturedSpills<'a> {
    fn new(capture: &'a EvidenceCapture, dataset: SnapshotDataset, family: EvidenceFamily) -> Result<Self> {
        references::validate_capture(capture)?;
        let read_id = cached_statement_read_id(capture.capture())?;
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
        let runtime = if dataset == SnapshotDataset::CachedRuntime {
            let observations = capture
                .records()
                .iter()
                .map(|record| match &record.row().record {
                    TelemetryRecord::Operational(row) if matches!(row.fact, OperationalFact::MssqlCachedRuntime(_)) => {
                        Ok(row.clone())
                    }
                    _ => Err(invalid()),
                })
                .collect::<Result<Vec<_>>>()?;
            let read = CachedRuntimeRead {
                origin: CachedRuntimeReadOrigin::captured(
                    capture.source(),
                    DiagnosticCaptureRef {
                        capture: capture.capture().clone(),
                        manifest_sha256: capture.capture_manifest_sha256().into(),
                        source_scope_id: capture.source().captured_range()?.scope_id,
                    },
                    capture
                        .records()
                        .iter()
                        .map(|record| {
                            let ordinal = record.row_ordinal().checked_sub(1).ok_or_else(invalid)?;
                            CapturedSnapshotPosition::from_export(record.row(), ordinal)
                        })
                        .collect::<Result<Vec<_>>>()?,
                )?,
                description: capture.source().clone(),
                observations,
                read_complete: capture.is_read_complete(),
            };
            if !read.is_valid() {
                return Err(invalid());
            }
            Some(read)
        } else {
            None
        };
        Ok(Self { capture, read_id, runtime, sources })
    }
    fn rows(&self) -> Result<Vec<CompletedSpillRecord<'_>>> {
        match &self.runtime {
            Some(read) => {
                if read.observations.len() != self.capture.records().len() {
                    return Err(invalid());
                }
                read.observations
                    .iter()
                    .zip(self.capture.records())
                    .map(|(row, record)| {
                        Ok(CompletedSpillRecord::captured_runtime(references::from_record(self.capture, record)?, row))
                    })
                    .collect()
            }
            None => self
                .capture
                .records()
                .iter()
                .map(|record| {
                    CompletedSpillRecord::captured(references::from_record(self.capture, record)?, &record.row().record)
                })
                .collect(),
        }
    }
    fn evidence<'s>(&'s self, rows: &'s [CompletedSpillRecord<'s>]) -> CompletedSpillEvidence<'s> {
        CompletedSpillEvidence {
            read_id: &self.read_id,
            source: self.runtime.as_ref().map(|read| &read.description).unwrap_or_else(|| self.capture.source()),
            rows,
            sources: &self.sources,
            read_complete: self
                .runtime
                .as_ref()
                .map_or_else(|| self.capture.is_read_complete(), |read| read.read_complete),
        }
    }
}
