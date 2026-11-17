//! A closed source result is one authenticated capture, never a diagnostic row roster.
use super::*;

impl Files {
    /// Exact saved-lineage lookup only. The caller still authenticates canonical result bytes and consumption
    /// before settling a lost Fleet acknowledgement; no current checkpoint/actor can replace this association.
    pub(in crate::outbox) fn consumed_source_read_result(
        &self,
        intent: &DiagnosticInvestigationIntent,
    ) -> Result<Option<DiagnosticCaptureRef>> {
        if !intent.is_valid() || !matches!(intent.focus, DiagnosticFocus::SourceRead { .. }) {
            return Err(integrity());
        }
        let lineage = DiagnosticInvestigationLineage::from_intent(intent)?;
        let encoded = encode_investigation(Some(&lineage))?.ok_or_else(integrity)?;
        let mut query = self
            .journal
            .conn
            .prepare(
                "SELECT parent_json FROM diagnostic_inputs WHERE controller_version=?1 AND acknowledged=1
             AND evidence_only=0 AND lineage_json=?2 LIMIT 2",
            )
            .map_err(storage)?;
        let rows = query
            .query_map(params![DIAGNOSTIC_CONTROLLER_VERSION, encoded], |row| row.get::<_, String>(0))
            .map_err(storage)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage)?;
        match rows.as_slice() {
            [] => Ok(None),
            [parent] => {
                let parent: DiagnosticCaptureRef = decode(parent)?;
                if !parent.is_valid() {
                    return Err(integrity());
                }
                Ok(Some(parent))
            }
            _ => Err(integrity()),
        }
    }

    pub(super) fn validate_source_read_capture(
        &self,
        event: &DiagnosticInvestigationEvent,
        input: &DiagnosticInput,
    ) -> Result<CollectionDescription> {
        let DiagnosticInvestigationOutcome::Captured { capture } = &event.outcome else { return Err(integrity()) };
        // The private input was constructed by the same canonical local verifier, or by authenticated
        // committed-range recovery. It attests a capture, not that history rows have been materialized here.
        if !event.is_valid()
            || input.parent != *capture
            || !input.rows.is_empty()
            || !matches!(input.source.observation, SourceObservation::SourceRead { .. })
            || !event.intent.matches_result_source(&input.source)
            || input
                .investigation
                .as_ref()
                .is_none_or(|lineage| !lineage.matches_intent(&event.intent.focus, &event.intent))
        {
            return Err(integrity());
        }
        Ok(input.source.clone())
    }

    pub(super) fn validate_source_read_revision(
        &self,
        before: Option<&DiagnosticEpisode>,
        revision: &DiagnosticRevision,
        result_source: Option<&CollectionDescription>,
    ) -> Result<()> {
        let Some(stage) = &revision.episode.source_read else { return Ok(()) };
        if before.and_then(|episode| episode.source_read.as_deref()) == Some(stage.as_ref()) {
            return Ok(());
        }
        match &stage.state {
            SourceReadStageState::Pending | SourceReadStageState::Deferred { .. } => (),
            SourceReadStageState::Collecting => {
                let event = revision.investigation.as_ref().ok_or_else(integrity)?;
                if !matches!(event.intent.focus, DiagnosticFocus::SourceRead { .. })
                    || !matches!(event.outcome, DiagnosticInvestigationOutcome::Admitted)
                {
                    return Err(integrity());
                }
            }
            SourceReadStageState::Stopped { issue } => {
                let event = revision.investigation.as_ref().ok_or_else(integrity)?;
                if !matches!(event.intent.focus, DiagnosticFocus::SourceRead { .. })
                    || !matches!(&event.outcome, DiagnosticInvestigationOutcome::Stopped { issue: actual } if actual == issue)
                {
                    return Err(integrity());
                }
            }
            SourceReadStageState::Captured { capture } => {
                let event = revision.investigation.as_ref().ok_or_else(integrity)?;
                let source = result_source.ok_or_else(integrity)?;
                let expected = before.ok_or_else(integrity)?.complete_source_read(&event.intent, source, capture)?;
                if expected.source_read != revision.episode.source_read {
                    return Err(integrity());
                }
            }
        }
        Ok(())
    }
}
