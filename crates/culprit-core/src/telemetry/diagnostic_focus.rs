//! A later lookup can explain retained evidence without rewriting its original native observation.
use super::*;
mod compiler_admission;
mod request_memory;
pub use compiler_admission::{DiagnosticCompilerRow, analyze_diagnostic_compilation_admission};
pub use request_memory::{DiagnosticMemoryRow, analyze_diagnostic_memory};

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
    analyze_focus(source, input, rows, previous, analyzed_at, None)
}

/// The caller authenticates the sealed intent, original parent and shared owner before this pure transition.
pub fn analyze_diagnostic_focus_with_intent(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    rows: &[DiagnosticFocusRow<'_>],
    previous: &DiagnosticCheckpoint,
    analyzed_at: &str,
    intent: &DiagnosticInvestigationIntent,
) -> Result<DiagnosticFocusUpdate> {
    if !intent.matches_result_source(source)
        || previous.target.telemetry_id()? != intent.focus.participant().target.telemetry_id()?
    {
        return Err(invalid());
    }
    analyze_focus(source, input, rows, previous, analyzed_at, Some(intent))
}

fn analyze_focus(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    rows: &[DiagnosticFocusRow<'_>],
    previous: &DiagnosticCheckpoint,
    analyzed_at: &str,
    intent: Option<&DiagnosticInvestigationIntent>,
) -> Result<DiagnosticFocusUpdate> {
    let SourceObservation::FocusedPageWait { snapshot, result } = &source.observation else {
        return Err(invalid());
    };
    if !source.observation.covers_request()
        || !input.is_valid()
        || !previous.is_valid()
        || previous.controller_version != DIAGNOSTIC_CONTROLLER_VERSION
        || input.source_scope_id != source.captured_range()?.scope_id
        || intent.is_none() && source.target.telemetry_id()? != previous.target.telemetry_id()?
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
        if before.analysis.needs_work()
            && before.matches_page_wait_focus(&result.focus)
            && intent.is_none_or(|intent| intent.matches_episode(before))
        {
            apply_result(&mut next, result, input, rows.first())?;
            if let Some(mut revision) = DiagnosticRevision::between(Some(before), next.clone(), analyzed_at)? {
                if !revision.inputs.contains(&result.focus.participant().parent) {
                    revision.inputs.push(result.focus.participant().parent.clone());
                }
                if let Some(DiagnosticInvestigationIntent {
                    execution: DiagnosticExecutionBinding::Shared { owner },
                    ..
                }) = intent
                    && !revision.inputs.contains(owner)
                {
                    revision.inputs.push(owner.as_ref().clone());
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
        target: previous.target.clone(),
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
        episode.report =
            Some(DiagnosticReport::PageWait { observation: row.reference.clone(), blocking_contexts: vec![] });
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
    // A delayed lookup remains historical evidence; it cannot clear or weaken a newer native observation.
    if ObservationBounds::new(&episode.last_seen_at, &result.observed_until).is_none() {
        return Ok(());
    }
    if episode.lifecycle != DiagnosticLifecycle::Resolved
        && result.source == FocusSourceState::Matched
        && result.request == PageWaitRequestState::Missing
    {
        episode.lifecycle = DiagnosticLifecycle::Resolved;
        episode.recovery = Some(DiagnosticRecoveryProof::FocusedParticipantGone {
            capture: input.clone(),
            parent: Box::new(result.focus.participant().parent.clone()),
        });
    } else if episode.lifecycle != DiagnosticLifecycle::Resolved && !result.permits_page_lookup() {
        episode.lifecycle = DiagnosticLifecycle::Uncertain;
    }
    Ok(())
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}
