//! Original runtime and optional definitions keep independent authenticated source/row/read contexts.
use super::query_store_definitions::QueryStoreDefinitionCapture;
use super::*;
use culprit_checks::page_server_reads::{PageServerReadEvidence, RuntimeEvidence};
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
    let rows = original_rows(capture.source(), &read, &context.records)?;
    let definition_capture = definitions.map(QueryStoreDefinitionCapture::new).transpose()?;
    let report = culprit_checks::page_server_reads::explain_archive(PageServerReadEvidence {
        source: capture.source(),
        read: &read,
        reference_coverage: capture.reference_coverage(),
        rows: &rows,
        selected_rows: capture.metrics().selected_rows,
        read_complete: capture.is_read_complete(),
        definitions: definition_capture.as_ref().map(QueryStoreDefinitionCapture::evidence),
    })?;
    Ok(CollectedPageServerReadsAnalysis {
        context,
        definition_context: definitions.map(|capture| Box::new(crate::collected_context::from_capture(capture))),
        report,
    })
}

/// Warehouse and local owners pass the same unchanged canonical records to the one pure renderer.
pub(crate) fn original_rows<'a>(
    source: &CollectionDescription,
    read: &CapturedReadRef,
    records: impl IntoIterator<Item = &'a culprit_core::collected_analysis::CollectedAnalysisRecord>,
) -> Result<Vec<RuntimeEvidence<'a>>> {
    let original = read.bind(source)?;
    records
        .into_iter()
        .map(|record| {
            let TelemetryRecord::QueryRuntime(runtime) = &record.row.record else { return Err(invalid()) };
            Ok(RuntimeEvidence { reference: references::analysis_reference(&original, record)?, runtime })
        })
        .collect()
}
