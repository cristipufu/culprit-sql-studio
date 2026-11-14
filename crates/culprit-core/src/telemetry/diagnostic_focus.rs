//! A later lookup can explain retained evidence without rewriting its original native observation.
use super::*;

pub struct DiagnosticFocusRow<'a> {
    pub reference: &'a DiagnosticEvidenceRef,
    pub observation: &'a PageWaitObservation,
}

pub struct DiagnosticFocusUpdate {
    pub revisions: Vec<DiagnosticRevision>,
    pub checkpoint: DiagnosticCheckpoint,
}

pub fn analyze_diagnostic_focus(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    rows: &[DiagnosticFocusRow<'_>],
    previous: &DiagnosticCheckpoint,
    analyzed_at: &str,
) -> Result<DiagnosticFocusUpdate> {
    let SourceObservation::FocusedPageWait { snapshot, result } = &source.observation else {
        return Err(invalid());
    };
    if !source.observation.covers_request()
        || !input.is_valid()
        || !previous.is_valid()
        || previous.controller_version != DIAGNOSTIC_CONTROLLER_VERSION
        || input.source_scope_id != source.captured_range()?.scope_id
        || source.target.telemetry_id()? != previous.target.telemetry_id()?
        || rows.len() > 1
        || rows.len() != snapshot.records as usize
    {
        return Err(invalid());
    }
    for row in rows {
        if row.reference.capture_ref() != *input || !result.accepts_observation(row.observation) {
            return Err(invalid());
        }
        let exported = ExportedRecord::new(
            source,
            &input.capture,
            TelemetryRecord::Operational(OperationalObservation {
                source: row.observation.source.clone(),
                key: row.observation.key()?,
                fact: OperationalFact::PageWait(Box::new(row.observation.clone())),
            }),
        )?;
        if DiagnosticEvidenceRef::from_export(source, &input.manifest_sha256, &exported, row.reference.row_ordinal)?
            != *row.reference
        {
            return Err(invalid());
        }
    }
    let mut revisions = Vec::new();
    let mut episodes = Vec::new();
    for before in &previous.episodes {
        let mut next = before.clone();
        if before.matches_focus(&result.focus) {
            apply_result(&mut next, result, input, rows.first())?;
            if let Some(mut revision) = DiagnosticRevision::between(Some(before), next.clone(), analyzed_at)? {
                if !revision.inputs.contains(&result.focus.parent) {
                    revision.inputs.push(result.focus.parent.clone());
                }
                revision.id = revision.expected_id()?;
                if !revision.is_valid() {
                    return Err(invalid());
                }
                next = revision.episode.clone();
                revisions.push(revision);
            }
        }
        if next.retained_in_checkpoint() {
            episodes.push(next);
        }
    }
    let checkpoint = DiagnosticCheckpoint {
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        target: source.target.clone(),
        input: input.clone(),
        episodes,
    };
    if !checkpoint.is_valid() {
        return Err(invalid());
    }
    Ok(DiagnosticFocusUpdate { revisions, checkpoint })
}

fn apply_result(
    episode: &mut DiagnosticEpisode,
    result: &ActivityWaitFocusResult,
    input: &DiagnosticCaptureRef,
    row: Option<&DiagnosticFocusRow<'_>>,
) -> Result<()> {
    if let Some(row) = row {
        episode.report = Some(DiagnosticReport::PageWait { observation: row.reference.clone() });
        episode.analysis = if row.observation.target.is_some() {
            DiagnosticAnalysisStatus::Analyzed
        } else {
            DiagnosticAnalysisStatus::Partial
        };
        episode.issue = match row.observation.resolution {
            PageWaitResolution::ObservedAtBothBounds => None,
            PageWaitResolution::Limit => Some(DiagnosticAnalysisIssue::BudgetExhausted),
            PageWaitResolution::Unsupported | PageWaitResolution::OutsideDatabase => {
                Some(DiagnosticAnalysisIssue::Unsupported)
            }
            _ => Some(DiagnosticAnalysisIssue::EvidenceUnavailable),
        };
    } else {
        episode.analysis = DiagnosticAnalysisStatus::Unresolved;
        episode.issue = Some(match (result.source, result.request) {
            (FocusSourceState::Changed, _) => DiagnosticAnalysisIssue::SourceChanged,
            (FocusSourceState::Matched, PageWaitRequestState::Missing | PageWaitRequestState::Changed) => {
                DiagnosticAnalysisIssue::NativeContextGone
            }
            _ => DiagnosticAnalysisIssue::EvidenceUnavailable,
        });
    }
    if episode.lifecycle != DiagnosticLifecycle::Resolved
        && result.source == FocusSourceState::Matched
        && result.request == PageWaitRequestState::Missing
    {
        episode.lifecycle = DiagnosticLifecycle::Resolved;
        episode.recovery = Some(DiagnosticRecoveryProof::FocusedParticipantGone {
            capture: input.clone(),
            parent: Box::new(result.focus.parent.clone()),
        });
    } else if episode.lifecycle != DiagnosticLifecycle::Resolved && !result.permits_page_lookup() {
        episode.lifecycle = DiagnosticLifecycle::Uncertain;
    }
    Ok(())
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}
