//! QueryDefinitions-only archive adapter shared by compiler and runtime statement links.
use super::*;
use culprit_checks::query_store_definitions::QueryStoreDefinitionRecord;

pub(super) fn rows(capture: &EvidenceCapture) -> Result<Vec<QueryStoreDefinitionRecord<'_>>> {
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
            Ok(QueryStoreDefinitionRecord { reference: references::from_record(capture, record)?, definition })
        })
        .collect()
}

/// Equal-range family selections must pin the identical original acquisition, not merely a logical namespace.
pub(super) fn validate_shared_capture(primary: &EvidenceCapture, definitions: &EvidenceCapture) -> Result<()> {
    if primary.range_id() == definitions.range_id()
        && (primary.capture() != definitions.capture()
            || primary.source() != definitions.source()
            || primary.capture_manifest_sha256() != definitions.capture_manifest_sha256()
            || primary.recovery_manifest_sha256() != definitions.recovery_manifest_sha256()
            || primary.collected_from() != definitions.collected_from()
            || primary.collected_until() != definitions.collected_until())
    {
        return Err(DeliveryProblem::InvalidContract.error());
    }
    Ok(())
}
