//! Recipe declarations explain withheld native questions; they never open a source or grant admission.
use super::*;
use std::collections::BTreeMap;
mod source_read;

/// Catalog and subject order have no policy meaning. Keep every descriptor field in its replay fingerprint.
pub(super) fn canonical_catalog(mut recipes: Vec<RecipeDescriptor>) -> Result<Vec<RecipeDescriptor>> {
    recipes.sort_by(|left, right| left.id.cmp(&right.id));
    for recipe in &mut recipes {
        let mut subjects = std::mem::take(&mut recipe.subjects)
            .into_iter()
            .map(|subject| {
                let key = serde_json::to_vec(&subject).map_err(|_| CollectionStop::InvalidPage.error())?;
                Ok((key, subject))
            })
            .collect::<Result<Vec<_>>>()?;
        subjects.sort_by(|left, right| left.0.cmp(&right.0));
        recipe.subjects = subjects.into_iter().map(|(_, subject)| subject).collect();
    }
    Ok(recipes)
}

pub(super) fn qualify_questions(
    input: &DiagnosticInput,
    assignment: Option<&DiagnosticAssignment>,
    update: &mut DiagnosticUpdate,
) -> Result<()> {
    let Some(assignment) = assignment else { return Ok(()) };
    if matches!(input.source.observation, SourceObservation::FocusedCompilationAdmission { .. }) {
        return source_read::qualify(input, assignment, update);
    }
    let Some(recipes) = &assignment.recipes else { return Ok(()) };
    if matches!(input.source.observation, SourceObservation::FocusedPageWait { .. }) {
        return qualify_writer_question(input, assignment, recipes, update);
    }
    if matches!(input.source.observation, SourceObservation::PageWaitTargets { original_activity: Some(_), .. }) {
        return qualify_task_questions(input, assignment, recipes, update);
    }
    if !matches!(input.source.observation, SourceObservation::Activity(_)) {
        return Ok(());
    }
    let mut rows = BTreeMap::new();
    for row in &input.rows {
        let TelemetryRecord::Activity(original) = &row.row.record else {
            return Err(CollectionStop::InvalidPage.error());
        };
        if rows.insert(&original.key, row).is_some() {
            return Err(CollectionStop::InvalidPage.error());
        }
    }
    for episode in &mut update.checkpoint.episodes {
        if !matches!(
            episode.signal,
            DiagnosticSignal::PageWait { .. }
                | DiagnosticSignal::LockWait { .. }
                | DiagnosticSignal::RequestMemory { .. }
                | DiagnosticSignal::CompilationAdmission { .. }
                | DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. }
        ) || !episode.analysis.needs_work()
            || episode.latest.capture_ref() != input.parent
        {
            continue;
        }
        let population =
            matches!(episode.signal, DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. })
                && long_running_question(recipes, &assignment.subject) == Some(RecipeSelector::PageWaitTasks);
        if population && episode.first.capture_ref() != input.parent {
            // This input can update symptom/lifecycle facts. It cannot replace the first frame's question or
            // erase its pending authority because a later point has different token visibility.
            if let Some(original) = input
                .checkpoint
                .iter()
                .flat_map(|checkpoint| &checkpoint.episodes)
                .find(|original| original.id == episode.id && original.first == episode.first)
            {
                episode.analysis = original.analysis;
                episode.issue = original.issue.clone();
            }
            continue;
        }
        let row = rows.get(episode.signal.activity_key()).ok_or_else(|| CollectionStop::InvalidPage.error())?;
        if row.reference != episode.latest {
            return Err(CollectionStop::InvalidPage.error());
        }
        let TelemetryRecord::Activity(original) = &row.row.record else {
            return Err(CollectionStop::InvalidPage.error());
        };
        let plan = if population {
            episode.plan_page_wait_tasks_focus(&input.activity_frame()?)?
        } else {
            episode.plan_focus(&input.source, original, &row.reference)?
        };
        let issue = match plan {
            DiagnosticFocusPlan::NotRequired => continue,
            DiagnosticFocusPlan::Unavailable(issue) => {
                episode.analysis = DiagnosticAnalysisStatus::Unresolved;
                issue
            }
            DiagnosticFocusPlan::Ready(focus) => {
                episode.analysis = DiagnosticAnalysisStatus::Deferred;
                qualification_issue(recipes, &assignment.subject, &focus)
            }
        };
        episode.issue = Some(issue);
    }
    update.step_id = digest(
        &serde_json::to_vec(&("diagnostic-recipe-policy-v1", &update.step_id, recipes))
            .map_err(|_| CollectionStop::InvalidPage.error())?,
    );
    rebuild_revisions(input, update)
}

/// A declared narrower request reader is not bypassed because its cost is unqualified or its evidence is
/// temporarily absent. Only explicit catalog absence permits the supported task-population question.
pub(crate) fn long_running_question(
    recipes: &[RecipeDescriptor],
    subject: &CollectionSubject,
) -> Option<RecipeSelector> {
    [RecipeSelector::CurrentRequestWork, RecipeSelector::PageWaitTasks].into_iter().find(|selector| {
        recipes.iter().any(|recipe| {
            recipe.purpose == RecipePurpose::Investigation
                && recipe.selector == *selector
                && recipe.subjects.iter().any(|declared| declared.accepts(subject))
        })
    })
}

fn qualification_issue(
    recipes: &[RecipeDescriptor],
    subject: &CollectionSubject,
    focus: &DiagnosticFocus,
) -> DiagnosticAnalysisIssue {
    let request = CollectionRequest {
        subject: subject.clone(),
        selection: match focus.activity_selection() {
            Some(selection) => selection,
            None => return DiagnosticAnalysisIssue::Unsupported,
        },
        budget: CollectionBudget::default(),
    };
    let recipe =
        recipes.iter().find(|recipe| recipe.purpose == RecipePurpose::Investigation && recipe.accepts(&request));
    declared_issue(recipe)
}

fn declared_issue(recipe: Option<&RecipeDescriptor>) -> DiagnosticAnalysisIssue {
    match recipe.map(|recipe| &recipe.cost) {
        Some(RecipeCostQualification::Unqualified) => DiagnosticAnalysisIssue::CostUnqualified,
        // A source-issued receipt still needs independent artifact authentication and the selected authority.
        Some(RecipeCostQualification::Measured(_)) => DiagnosticAnalysisIssue::EvidenceUnavailable,
        None => DiagnosticAnalysisIssue::Unsupported,
    }
}

fn qualify_writer_question(
    input: &DiagnosticInput,
    assignment: &DiagnosticAssignment,
    recipes: &[RecipeDescriptor],
    update: &mut DiagnosticUpdate,
) -> Result<()> {
    let SourceObservation::FocusedPageWait { result, .. } = &input.source.observation else {
        return Ok(());
    };
    if !input.rows.iter().any(|row| {
        matches!(&row.row.record, TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::PageWait(page), ..
        }) if supports_page_wait_writers(&input.source, page, &result.focus))
    }) {
        return Ok(());
    }
    let recipe = recipes.iter().find(|recipe| {
        recipe.purpose == RecipePurpose::Investigation
            && recipe.selector == RecipeSelector::FocusedPageWaitWriters
            && recipe.subjects.iter().any(|subject| subject.accepts(&assignment.subject))
    });
    for episode in &mut update.checkpoint.episodes {
        if episode.analysis.needs_work()
            && matches!(&episode.report, Some(DiagnosticReport::PageWait { observation, .. }) if observation.capture_ref() == input.parent)
        {
            episode.analysis = DiagnosticAnalysisStatus::Deferred;
            episode.issue = Some(declared_issue(recipe));
        }
    }
    update.step_id = digest(
        &serde_json::to_vec(&("diagnostic-writer-recipe-policy-v1", &update.step_id, recipes))
            .map_err(|_| CollectionStop::InvalidPage.error())?,
    );
    rebuild_revisions(input, update)
}

fn rebuild_revisions(input: &DiagnosticInput, update: &mut DiagnosticUpdate) -> Result<()> {
    let previous = input
        .checkpoint
        .iter()
        .flat_map(|checkpoint| &checkpoint.episodes)
        .map(|episode| (episode.id.as_str(), episode))
        .collect::<BTreeMap<_, _>>();
    let mut revisions = std::mem::take(&mut update.revisions)
        .into_iter()
        .map(|revision| (revision.episode.id.clone(), revision))
        .collect::<BTreeMap<_, _>>();
    for episode in &mut update.checkpoint.episodes {
        let observed_revision = revisions.remove(&episode.id);
        if let Some(mut revision) = DiagnosticRevision::between(
            previous.get(episode.id.as_str()).copied(),
            episode.clone(),
            &update.analyzed_at,
        )? {
            // Recipe wording/status changes do not remove the authenticated native prerequisites already used
            // by the observer, including a paired population's exact first Activity receipt.
            if let Some(observed) = observed_revision {
                for reference in observed.inputs {
                    if !revision.inputs.contains(&reference) {
                        revision.inputs.push(reference);
                    }
                }
            }
            revision.id = revision.expected_id()?;
            if !revision.is_valid() {
                return Err(DeliveryProblem::Integrity.error());
            }
            *episode = revision.episode.clone();
            revisions.insert(episode.id.clone(), revision);
        }
    }
    update.checkpoint.episodes.retain(DiagnosticEpisode::retained_in_checkpoint);
    // Revisions for terminal episodes already omitted by the observer remain in this immutable history.
    update.revisions = revisions.into_values().collect();
    if !update.checkpoint.is_valid() {
        return Err(CollectionStop::InvalidPage.error());
    }
    Ok(())
}

fn qualify_task_questions(
    input: &DiagnosticInput,
    assignment: &DiagnosticAssignment,
    recipes: &[RecipeDescriptor],
    update: &mut DiagnosticUpdate,
) -> Result<()> {
    let recipe = recipes.iter().find(|recipe| {
        recipe.purpose == RecipePurpose::Investigation
            && recipe.selector == RecipeSelector::FocusedPageTaskHolders
            && recipe.subjects.iter().any(|subject| subject.accepts(&assignment.subject))
    });
    for episode in &mut update.checkpoint.episodes {
        if episode.analysis.needs_work()
            && episode.latest.capture_ref() == input.parent
            && matches!(episode.signal, DiagnosticSignal::PageTaskWait { .. })
        {
            episode.analysis = DiagnosticAnalysisStatus::Deferred;
            episode.issue = Some(declared_issue(recipe));
        }
    }
    update.step_id = digest(
        &serde_json::to_vec(&("diagnostic-task-recipe-policy-v1", &update.step_id, recipes))
            .map_err(|_| CollectionStop::InvalidPage.error())?,
    );
    rebuild_revisions(input, update)
}
