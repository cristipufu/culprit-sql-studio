//! Immutable revisions are history, not a reconstruction of the live controller's working set.
use culprit_core::{
    IncidentHistoryEntry, IncidentHistoryEvidence, IncidentHistoryProvenance, IncidentHistoryReport,
    MAX_INCIDENT_HISTORY_REVISIONS, Result, SourceState, format, t, telemetry::*,
};
use std::collections::BTreeMap;
mod compilation;
mod temporary_resources;

/// Authenticated callers supply the receipt for this exact revision body. Imported rows deliberately have none.
pub struct HistoryRevision<'a> {
    pub revision: &'a DiagnosticRevision,
    pub original: Option<&'a DiagnosticEvidenceRef>,
}

pub fn explain(
    selected: &[HistoryRevision<'_>],
    provenance: IncidentHistoryProvenance,
    read_complete: bool,
) -> Result<IncidentHistoryReport> {
    // Inspect the complete bounded selection before the display cap: no plausible survivor of a contradiction.
    if selected.len() > MAX_DIAGNOSTIC_EPISODES {
        return Err(DeliveryProblem::Capacity.error());
    }
    let mut revisions = BTreeMap::new();
    for item in selected {
        let revision = item.revision;
        let number = revision.episode.revision.parse::<u64>().map_err(|_| invalid())?;
        if !revision.is_valid() || number == 0 || !receipt_matches(item, provenance) {
            return Err(invalid());
        }
        let key = (revision.controller_version, &revision.episode.id, number);
        if let Some(previous) = revisions.insert(key, item)
            && (previous.revision != revision || previous.original != item.original)
        {
            return Err(DeliveryProblem::Conflict.error());
        }
    }
    if revisions.len() > MAX_INCIDENT_HISTORY_REVISIONS {
        return Err(DeliveryProblem::Capacity.error());
    }
    let entries = revisions
        .iter()
        .map(|((controller, episode, number), item)| {
            let predecessor_selected = *number == 1 || revisions.contains_key(&(*controller, *episode, *number - 1));
            entry(item, provenance, predecessor_selected)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut limitations = vec![
        t!("view.incident_history.limit.selection"),
        t!("view.incident_history.limit.groups"),
        t!("view.incident_history.limit.clocks"),
        t!("view.incident_history.limit.references"),
        t!("view.incident_history.limit.actions"),
    ];
    limitations.push(match provenance {
        IncidentHistoryProvenance::AuthenticatedCapture => t!("view.incident_history.limit.authenticated"),
        IncidentHistoryProvenance::ImportedArtifact => t!("view.incident_history.limit.imported"),
    });
    if !read_complete {
        limitations.push(t!("view.incident_history.limit.partial"));
    }
    let source_state = match provenance {
        IncidentHistoryProvenance::AuthenticatedCapture => {
            Some(if read_complete { SourceState::Collected } else { SourceState::Partial })
        }
        IncidentHistoryProvenance::ImportedArtifact => None,
    };
    Ok(IncidentHistoryReport {
        provenance,
        selected_read_complete: read_complete,
        source_state,
        source_reason: None,
        title: t!("view.incident_history.title"),
        summary: t!("view.incident_history.summary", count = format::digits(entries.len().to_string())),
        limitations,
        entries,
    })
}

fn receipt_matches(item: &HistoryRevision<'_>, provenance: IncidentHistoryProvenance) -> bool {
    match (provenance, item.original) {
        (IncidentHistoryProvenance::ImportedArtifact, None) => true,
        (IncidentHistoryProvenance::AuthenticatedCapture, Some(reference)) => {
            reference.is_valid()
                && matches!(&reference.native_key, NativeRecordIdentity::Operational { source, key }
                if source == DIAGNOSTICS_SOURCE && key == &item.revision.id)
        }
        _ => false,
    }
}

fn entry(
    item: &HistoryRevision<'_>,
    provenance: IncidentHistoryProvenance,
    predecessor_selected: bool,
) -> Result<IncidentHistoryEntry> {
    let revision = item.revision;
    let episode = &revision.episode;
    let evidence = evidence_links(revision, provenance);
    let reasons = recorded_reasons(revision, predecessor_selected);
    Ok(IncidentHistoryEntry {
        controller_version: revision.controller_version,
        revision_id: revision.id.clone(),
        episode_id: episode.id.clone(),
        target_id: episode.scope.target.telemetry_id()?,
        target_scope: episode.scope.target.measurement_scope(),
        scope_id: episode.scope.key()?,
        source: episode.scope.source.clone(),
        scope: episode.scope.measurement_scope,
        scope_label: t!(
            "view.incident_history.native_scope",
            scope = match episode.scope.measurement_scope {
                culprit_core::MeasurementScope::Database => "database",
                culprit_core::MeasurementScope::Pool => "pool",
                culprit_core::MeasurementScope::Node => "node",
                culprit_core::MeasurementScope::Cluster => "cluster",
                culprit_core::MeasurementScope::Server => "server",
                culprit_core::MeasurementScope::Session => "session",
                culprit_core::MeasurementScope::File => "file",
                culprit_core::MeasurementScope::Unknown => "unknown",
            }
        ),
        revision: episode.revision.clone(),
        previous_revision: revision.previous_revision.clone(),
        predecessor_selected,
        lifecycle: episode.lifecycle,
        analysis: episode.analysis,
        title: t!(
            "view.incident_history.revision",
            condition = question_label(&episode.signal),
            revision = format::digits(episode.revision.clone())
        ),
        phase: t!(
            "view.incident_history.phase",
            lifecycle = lifecycle(episode.lifecycle),
            analysis = analysis(episode.analysis)
        ),
        first_seen_at: episode.first_seen_at.clone(),
        last_seen_at: episode.last_seen_at.clone(),
        recorded_at: revision.analyzed_at.clone(),
        original: item.original.cloned(),
        evidence,
        input_references: revision.inputs.len() as u32,
        followup_recipe: revision.investigation.as_ref().map(|event| event.intent.recipe_id.clone()),
        followup_recipe_version: revision.investigation.as_ref().map(|event| event.intent.recipe_version),
        issue: episode.issue.clone(),
        reasons,
        next_step: if episode.source_read.as_deref().is_some_and(|stage| {
            stage.focus.question == SourceReadQuestion::TemporarySpaceContributors
                && matches!(stage.state, SourceReadStageState::Answered { .. })
        }) {
            t!("view.tempdb_question.next")
        } else if episode.analysis == DiagnosticAnalysisStatus::Analyzed {
            t!("view.incident_history.next.analyzed")
        } else {
            t!("view.incident_history.next.incomplete")
        },
    })
}
fn evidence_links(
    revision: &DiagnosticRevision,
    provenance: IncidentHistoryProvenance,
) -> Vec<IncidentHistoryEvidence> {
    let episode = &revision.episode;
    let availability = match provenance {
        IncidentHistoryProvenance::AuthenticatedCapture => t!("view.incident_history.evidence.referenced"),
        IncidentHistoryProvenance::ImportedArtifact => t!("view.incident_history.evidence.unverified"),
    };
    let mut evidence = vec![
        link(t!("view.incident_history.evidence.first"), &episode.first, &availability),
        link(t!("view.incident_history.evidence.latest"), &episode.latest, &availability),
    ];
    if let Some(peak) = &episode.peak {
        evidence.push(link(t!("view.incident_history.evidence.peak"), peak, &availability));
    }
    if let Some(proof) = &episode.recovery {
        evidence.push(IncidentHistoryEvidence {
            label: t!("view.incident_history.evidence.recovery"),
            capture: proof.capture().clone(),
            reference: None,
            availability: availability.clone(),
        });
    }
    if let Some(stage) = &episode.source_read
        && let Some(capture) = stage.result()
    {
        evidence.push(IncidentHistoryEvidence {
            label: t!("view.incident_history.evidence.followup"),
            capture: capture.clone(),
            reference: None,
            availability: availability.clone(),
        });
    }
    if let Some(report) = &episode.report {
        evidence.push(link(t!("view.incident_history.evidence.analysis"), report.evidence(), &availability));
    }
    if let Some(event) = &revision.investigation
        && let DiagnosticInvestigationOutcome::Captured { capture } = &event.outcome
    {
        evidence.push(IncidentHistoryEvidence {
            label: t!("view.incident_history.evidence.attempt"),
            capture: capture.clone(),
            reference: None,
            availability: availability.clone(),
        });
    }
    evidence
}
fn recorded_reasons(revision: &DiagnosticRevision, predecessor_selected: bool) -> Vec<String> {
    let episode = &revision.episode;
    let mut reasons = Vec::new();
    if let Some(issue) = &episode.issue {
        reasons.push(issue_message(issue));
    }
    if !predecessor_selected {
        reasons.push(t!("view.incident_history.limit.predecessor"));
    }
    if let Some(stage) = &episode.source_read {
        reasons.push(t!(
            "view.incident_history.source_stage",
            state = match &stage.state {
                SourceReadStageState::Pending => "pending",
                SourceReadStageState::Deferred { .. } => "deferred",
                SourceReadStageState::Collecting => "collecting",
                SourceReadStageState::Captured { .. } => "captured",
                SourceReadStageState::Answered { .. } => "answered",
                SourceReadStageState::Recovered => "recovered",
                SourceReadStageState::Stopped { .. } => "stopped",
            }
        ));
        if stage.focus.question == SourceReadQuestion::TemporarySpaceContributors {
            reasons.push(t!("view.tempdb_question.question"));
        }
        if matches!(stage.focus.prerequisite, SourceReadPrerequisite::ObservedQueryHistoryFailure { .. }) {
            reasons.push(t!("view.incident_history.compilation.question"));
        }
        if let SourceReadStageState::Answered { answer } = &stage.state {
            match answer.as_ref() {
                SourceReadAnswer::TemporaryResources(answer) => reasons.extend(temporary_resources::explain(answer)),
                SourceReadAnswer::QueryCompilation(answer) => compilation::append(answer, &mut reasons),
            }
        }
        match &stage.state {
            SourceReadStageState::Deferred { issue } | SourceReadStageState::Stopped { issue } => {
                reasons.push(issue_message(issue))
            }
            _ => (),
        }
    }
    if revision.investigation.is_none() && episode.investigation_event.is_some() {
        reasons.push(t!("view.incident_history.limit.prior_attempt"));
    }
    if let Some(event) = &revision.investigation {
        reasons.push(t!(
            "view.incident_history.investigation",
            outcome = match &event.outcome {
                DiagnosticInvestigationOutcome::Admitted => "admitted",
                DiagnosticInvestigationOutcome::Stopped { .. } => "stopped",
                DiagnosticInvestigationOutcome::Captured { .. } => "captured",
            },
            settlement = match event.source_work {
                SourceWorkState::NotStarted => "not-started",
                SourceWorkState::Active => "active",
                SourceWorkState::Quiescent => "quiescent",
                SourceWorkState::Unconfirmed => "unconfirmed",
            }
        ));
        if let DiagnosticInvestigationOutcome::Stopped { issue } = &event.outcome {
            reasons.push(issue_message(issue));
        }
    }
    if episode.report.is_some() {
        reasons.push(t!("view.incident_history.analysis_retained"));
    }
    reasons
}
fn link(label: String, reference: &DiagnosticEvidenceRef, availability: &str) -> IncidentHistoryEvidence {
    IncidentHistoryEvidence {
        label,
        capture: reference.capture_ref(),
        reference: Some(reference.clone()),
        availability: availability.into(),
    }
}
// Names the controller's existing typed question, without reinterpreting a native wait or attributing a cause.
fn question_label(signal: &DiagnosticSignal) -> String {
    if matches!(
        signal,
        DiagnosticSignal::OriginalObservation {
            condition: DiagnosticOriginalCondition::MssqlCleanupBoundary { .. },
            ..
        }
    ) {
        return t!("view.tempdb_question.question");
    }
    let kind = match signal {
        DiagnosticSignal::OriginalObservation { .. } => "other",
        DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. } => "long-running",
        DiagnosticSignal::Activity { condition: ActivityCondition::Blocked, .. } => "blocked",
        DiagnosticSignal::Activity { condition: ActivityCondition::IdleBlocker, .. } => "idle-blocker",
        DiagnosticSignal::LockWait { .. } => "lock",
        DiagnosticSignal::RequestMemory { .. } => "memory",
        DiagnosticSignal::CompilationAdmission { .. } => "compilation",
        DiagnosticSignal::LogRateGovernance { .. } => "log",
        DiagnosticSignal::PageWait { .. } => "page",
        DiagnosticSignal::PageTaskWait { .. } => "task-page",
    };
    t!("view.incident_history.condition", kind = kind)
}
fn lifecycle(value: DiagnosticLifecycle) -> &'static str {
    match value {
        DiagnosticLifecycle::Active => "active",
        DiagnosticLifecycle::Uncertain => "uncertain",
        DiagnosticLifecycle::Resolved => "resolved",
    }
}
fn analysis(value: DiagnosticAnalysisStatus) -> &'static str {
    match value {
        DiagnosticAnalysisStatus::Pending => "pending",
        DiagnosticAnalysisStatus::Deferred => "deferred",
        DiagnosticAnalysisStatus::Collecting => "collecting",
        DiagnosticAnalysisStatus::Partial => "partial",
        DiagnosticAnalysisStatus::Analyzed => "analyzed",
        DiagnosticAnalysisStatus::Unresolved => "unresolved",
    }
}
fn issue_message(issue: &DiagnosticAnalysisIssue) -> String {
    let reason = match issue {
        DiagnosticAnalysisIssue::CostUnqualified => "cost",
        DiagnosticAnalysisIssue::PolicyChanged => "policy",
        DiagnosticAnalysisIssue::MissingNativeIdentity => "identity",
        DiagnosticAnalysisIssue::NativeContextGone => "gone",
        DiagnosticAnalysisIssue::SourceChanged => "changed",
        DiagnosticAnalysisIssue::SourceUnavailable {
            code: Some(culprit_core::ErrorCode::PermissionDenied), ..
        } => "denied",
        DiagnosticAnalysisIssue::SourceUnavailable { .. } => "source",
        DiagnosticAnalysisIssue::BudgetExhausted => "budget",
        DiagnosticAnalysisIssue::Unsupported => "unsupported",
        DiagnosticAnalysisIssue::EvidenceUnavailable => "evidence",
        DiagnosticAnalysisIssue::Interrupted => "interrupted",
        DiagnosticAnalysisIssue::Capacity => "capacity",
    };
    t!("view.incident_history.issue", reason = reason)
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
mod tests;
