//! Original runtime and optional definitions keep independent authenticated source/row/read contexts.
use super::*;
use culprit_checks::{
    page_server_reads::{PageServerReadEvidence, RuntimeEvidence},
    query_store_definitions::QueryStoreDefinitionEvidence,
};
use culprit_core::page_server_read_report::CollectedPageServerReadsAnalysis;

pub fn page_server_reads(
    capture: &EvidenceCapture,
    definitions: Option<&EvidenceCapture>,
) -> Result<CollectedPageServerReadsAnalysis> {
    if let Some(definitions) = definitions {
        super::query_store_definitions::validate_shared_capture(capture, definitions)?;
    }
    references::validate_capture(capture)?;
    if capture.families() != [EvidenceFamily::QueryRuntime] || capture.source().query_history().is_none() {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let context = crate::collected_context::from_capture(capture);
    let read = references::captured_read(capture)?;
    let original = read.bind(capture.source())?;
    let rows = context
        .records
        .iter()
        .map(|record| {
            let TelemetryRecord::QueryRuntime(runtime) = &record.row.record else { return Err(invalid()) };
            let reference = references::analysis_reference(&original, record)?;
            Ok(RuntimeEvidence { reference, runtime })
        })
        .collect::<Result<Vec<_>>>()?;
    let definition_rows = definitions.map(super::query_store_definitions::rows).transpose()?;
    let definition_evidence =
        definitions.zip(definition_rows.as_deref()).map(|(capture, rows)| QueryStoreDefinitionEvidence {
            source: capture.source(),
            rows,
            selected_rows: capture.metrics().selected_rows,
            read_complete: capture.is_read_complete(),
        });
    let report = culprit_checks::page_server_reads::explain_archive(PageServerReadEvidence {
        source: capture.source(),
        read: &read,
        reference_coverage: capture.reference_coverage(),
        rows: &rows,
        selected_rows: capture.metrics().selected_rows,
        read_complete: capture.is_read_complete(),
        definitions: definition_evidence,
    })?;
    Ok(CollectedPageServerReadsAnalysis {
        context,
        definition_context: definitions.map(|capture| Box::new(crate::collected_context::from_capture(capture))),
        report,
    })
}
