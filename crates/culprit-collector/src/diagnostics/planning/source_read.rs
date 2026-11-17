//! A genuine retained gateway answer can owe a separate finite context question, even after its wait is quiet.
use super::*;

pub(super) fn qualify(
    input: &DiagnosticInput,
    assignment: &DiagnosticAssignment,
    update: &mut DiagnosticUpdate,
) -> Result<()> {
    let question = SourceReadQuestion::CompilationHistory;
    let recipe = assignment.recipes.as_deref().and_then(|recipes| {
        recipes.iter().find(|recipe| {
            recipe.purpose == RecipePurpose::Investigation
                && recipe.selector == question.selector()
                && recipe.subjects.iter().any(|subject| subject.accepts(&CollectionSubject::Database))
        })
    });
    let issue = if assignment.recipes.is_some() {
        declared_issue(recipe)
    } else {
        DiagnosticAnalysisIssue::SourceUnavailable { code: None, stopped: None }
    };
    for episode in &mut update.checkpoint.episodes {
        if episode.source_read.is_some()
            || !matches!(&episode.report, Some(DiagnosticReport::CompilationAdmission { report })
                if report.observation.capture_ref() == input.parent)
        {
            continue;
        }
        let Some(DiagnosticReport::CompilationAdmission { report }) = &episode.report else {
            return Err(CollectionStop::InvalidPage.error());
        };
        let focus = SourceReadFocus {
            basis: Box::new(episode.first.clone()),
            target: episode.scope.target.clone(),
            question,
            prerequisite: report.clone(),
        };
        *episode = episode
            .enroll_source_read(focus)?
            .transition_source_read(SourceReadStageState::Deferred { issue: issue.clone() })?;
    }
    update.step_id = digest(
        &serde_json::to_vec(&("diagnostic-source-read-policy-v1", &update.step_id, &assignment.recipes))
            .map_err(|_| CollectionStop::InvalidPage.error())?,
    );
    rebuild_revisions(input, update)
}
