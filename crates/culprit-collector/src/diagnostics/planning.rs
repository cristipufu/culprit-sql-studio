//! Recipe declarations explain withheld native questions; they never open a source or grant admission.
use super::*;
use std::collections::BTreeMap;

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
    let Some(recipes) = &assignment.recipes else { return Ok(()) };
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
        if !matches!(episode.signal, DiagnosticSignal::PageWait { .. })
            || !episode.analysis.needs_work()
            || episode.latest.capture_ref() != input.parent
        {
            continue;
        }
        let row = rows.get(episode.signal.activity_key()).ok_or_else(|| CollectionStop::InvalidPage.error())?;
        if row.reference != episode.latest {
            return Err(CollectionStop::InvalidPage.error());
        }
        let TelemetryRecord::Activity(original) = &row.row.record else {
            return Err(CollectionStop::InvalidPage.error());
        };
        let issue = match episode.plan_page_wait_focus(&input.source, original, &row.reference)? {
            PageWaitFocusPlan::NotRequired => continue,
            PageWaitFocusPlan::Unavailable(issue) => {
                episode.analysis = DiagnosticAnalysisStatus::Unresolved;
                issue
            }
            PageWaitFocusPlan::Ready(focus) => {
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

fn qualification_issue(
    recipes: &[RecipeDescriptor],
    subject: &CollectionSubject,
    focus: &ActivityWaitFocus,
) -> DiagnosticAnalysisIssue {
    let request = CollectionRequest {
        subject: subject.clone(),
        selection: CollectionSelection::FocusedPageWait { focus: Box::new(focus.clone()) },
        budget: CollectionBudget::default(),
    };
    let recipe =
        recipes.iter().find(|recipe| recipe.purpose == RecipePurpose::Investigation && recipe.accepts(&request));
    match recipe.map(|recipe| &recipe.cost) {
        Some(RecipeCostQualification::Unqualified) => DiagnosticAnalysisIssue::CostUnqualified,
        // A source-issued receipt still needs independent artifact authentication and the selected authority.
        Some(RecipeCostQualification::Measured(_)) => DiagnosticAnalysisIssue::EvidenceUnavailable,
        None => DiagnosticAnalysisIssue::Unsupported,
    }
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
        revisions.remove(&episode.id);
        if let Some(revision) = DiagnosticRevision::between(
            previous.get(episode.id.as_str()).copied(),
            episode.clone(),
            &update.analyzed_at,
        )? {
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
