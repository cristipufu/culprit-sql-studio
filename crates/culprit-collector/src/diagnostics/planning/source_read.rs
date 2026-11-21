//! Original incident evidence can owe a finite context question, even after its wait is quiet.
use super::*;

pub(super) fn qualify(
    input: &DiagnosticInput,
    assignment: &DiagnosticAssignment,
    update: &mut DiagnosticUpdate,
) -> Result<()> {
    for episode in &mut update.checkpoint.episodes {
        if episode.lifecycle == DiagnosticLifecycle::Resolved {
            if matches!(
                episode.signal,
                DiagnosticSignal::OriginalObservation {
                    condition: DiagnosticOriginalCondition::MssqlCleanupBoundary { .. },
                    ..
                }
            ) {
                continue;
            }
        }
        if episode.source_read.as_deref().is_some_and(|stage| !stage.needs_admission()) {
            continue;
        }
        let Some(focus) = episode.source_read_focus()? else { continue };
        let original_input = match &focus.prerequisite {
            SourceReadPrerequisite::CompilationAdmission { report } => report.observation.capture_ref(),
            SourceReadPrerequisite::ObservedQueryHistoryFailure { .. }
            | SourceReadPrerequisite::ObservedLogRateWait => episode.first.capture_ref(),
            SourceReadPrerequisite::ObservedCleanupBoundary { .. } => focus.basis.capture_ref(),
        };
        if episode.source_read.is_none() && original_input != input.parent {
            continue;
        }
        if episode.source_read.is_none() && focus.question == SourceReadQuestion::TemporarySpaceContributors {
            let Some(row) = input.rows.iter().find(|row| row.reference == *focus.basis) else { continue };
            // The native body, not boundary presence or cost qualification, satisfies the shared attention policy.
            if !focus.matches_original(&input.source, &row.row) {
                continue;
            }
        }
        let recipe = assignment.recipes.as_deref().and_then(|recipes| {
            recipes.iter().find(|recipe| {
                recipe.purpose == RecipePurpose::Investigation
                    && recipe.selector == focus.question.selector()
                    && recipe.subjects.iter().any(|subject| subject.accepts(&CollectionSubject::Database))
            })
        });
        let issue = if assignment.recipes.is_some() {
            declared_issue(recipe)
        } else {
            DiagnosticAnalysisIssue::SourceUnavailable { code: None, stopped: None }
        };
        *episode =
            episode.enroll_source_read(focus)?.transition_source_read(SourceReadStageState::Deferred { issue })?;
    }
    update.step_id = digest(
        &serde_json::to_vec(&("diagnostic-source-read-policy-v1", &update.step_id, &assignment.recipes))
            .map_err(|_| CollectionStop::InvalidPage.error())?,
    );
    rebuild_revisions(input, update)
}
