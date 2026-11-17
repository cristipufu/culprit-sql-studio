//! Prepare bounded questions from retained native evidence. This module never opens a source or grants admission.
use super::{RouteBindings, evidence::resolve_optional_evidence, routes::RouteBinding};
use crate::{
    capture::CollectionCancellation,
    delivery::recovery::RecoveryStore,
    fleet::{FleetBackend, FleetJournal, journal::FocusedStage, scheduler::FocusedJob},
    outbox::{DiagnosticInput, Outbox},
};
use culprit_core::{ComponentVersion, ErrorCode, Result, telemetry::*};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
mod page_tasks;
mod source_reads;
mod task_holders;
mod writers;

const MAX_CHECKPOINTS_PER_PASS: usize = 8;
const MAX_EPISODES_PER_PASS: usize = 128;
const MAX_QUESTIONS_PER_PASS: usize = 8;
pub(super) const PLANNING_TIME: Duration = Duration::from_secs(2);
const FOCUSED_COOLDOWN_SECONDS: u32 = 60;

pub(super) struct PreparedInvestigation {
    pub intent: DiagnosticInvestigationIntent,
    pub job: FocusedJob,
    pub route_revision: String,
}

struct EvidencePlanner<'a> {
    backend: &'a dyn FleetBackend,
    journal: &'a FleetJournal,
    recovery: Option<&'a dyn RecoveryStore>,
    cancellation: &'a CollectionCancellation,
    population_frames: BTreeMap<String, DiagnosticInput>,
    execution_owners: BTreeMap<String, NativeSourceReadScope>,
}

struct SelectedRoute {
    recipe: RecipeDescriptor,
    component: ComponentVersion,
    revision: String,
}

pub(super) async fn prepare(
    backend: &dyn FleetBackend,
    journal: &FleetJournal,
    outbox: &mut Outbox,
    recovery: Option<&dyn RecoveryStore>,
    targets: &[FleetTarget],
    bindings: &RouteBindings,
    cancellation: &CollectionCancellation,
) -> Result<Vec<PreparedInvestigation>> {
    if cancellation.is_cancelled() {
        return Ok(Vec::new());
    }
    if targets.len() > MAX_FLEET_TARGETS {
        return Err(DeliveryProblem::Capacity.error());
    }
    let stopped = CollectionCancellation::default();
    let finished = CollectionCancellation::default();
    let work = async {
        let result = bounded(backend, journal, outbox, recovery, targets, bindings, &stopped).await;
        finished.cancel();
        result
    };
    let relay = async {
        tokio::select! { biased;
            _ = cancellation.cancelled() => stopped.cancel(),
            _ = tokio::time::sleep(PLANNING_TIME) => stopped.cancel(),
            _ = finished.cancelled() => {},
        }
    };
    // Cancel read-only recovery work through its owner. A cache commit already begun is awaited, never dropped
    // to satisfy a planner timer; there is no detached cancellation task or native source future here.
    let (result, ()) = tokio::join!(work, relay);
    match result {
        Ok(_) if cancellation.is_cancelled() => Ok(Vec::new()),
        result => result,
    }
}

async fn bounded(
    backend: &dyn FleetBackend,
    journal: &FleetJournal,
    outbox: &mut Outbox,
    recovery: Option<&dyn RecoveryStore>,
    targets: &[FleetTarget],
    bindings: &RouteBindings,
    cancellation: &CollectionCancellation,
) -> Result<Vec<PreparedInvestigation>> {
    let mut prepared = Vec::new();
    let mut seen = BTreeSet::new();
    let mut populations = BTreeSet::new();
    let mut routes = BTreeMap::new();
    let (mut cursor, mut offset) = bindings.planning_position();
    let mut scanned = 0;
    let mut questions = 0;
    let mut evidence = EvidencePlanner {
        backend,
        journal,
        recovery,
        cancellation,
        population_frames: BTreeMap::new(),
        execution_owners: BTreeMap::new(),
    };
    for _ in 0..MAX_CHECKPOINTS_PER_PASS {
        if cancellation.is_cancelled() {
            break;
        }
        let page = outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, cursor.as_deref(), 1).await?;
        let Some(checkpoint) = page.checkpoints.first() else {
            bindings.set_planning_position(None, 0);
            break;
        };
        while offset < checkpoint.episodes.len() {
            if cancellation.is_cancelled() || scanned == MAX_EPISODES_PER_PASS || questions == MAX_QUESTIONS_PER_PASS {
                return Ok(prepared);
            }
            let episode = &checkpoint.episodes[offset];
            scanned += 1;
            if episode.has_owed_question()
                && matches!(
                    episode.signal,
                    DiagnosticSignal::PageWait { .. }
                        | DiagnosticSignal::PageTaskWait { .. }
                        | DiagnosticSignal::LockWait { .. }
                        | DiagnosticSignal::RequestMemory { .. }
                        | DiagnosticSignal::CompilationAdmission { .. }
                        | DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. }
                )
            {
                let mut selected_resources = BTreeSet::new();
                for target in targets {
                    if cancellation.is_cancelled() || questions == MAX_QUESTIONS_PER_PASS {
                        return Ok(prepared);
                    }
                    let Some(binding) = bindings.get(&target.id).filter(|binding| {
                        binding.matches_episode(&episode.scope) && binding.observed_after(&episode.last_seen_at)
                    }) else {
                        continue;
                    };
                    if binding.budget(target).is_none() {
                        continue;
                    }
                    if !selected_resources.insert(binding.source.target.telemetry_id()?) {
                        continue;
                    }
                    // Charge attempts before metadata or archive work, including withheld/absent questions.
                    questions += 1;
                    let mut available = Vec::new();
                    for selector in possible_questions(episode) {
                        let route_key = (
                            target.id.clone(),
                            serde_json::to_vec(&selector).map_err(|_| CollectionStop::InvalidPage.error())?,
                        );
                        if !routes.contains_key(&route_key) {
                            routes
                                .insert(route_key.clone(), select_route(backend, target, &binding.revision, selector)?);
                        }
                        if routes.get(&route_key).is_some_and(Option::is_some) {
                            available.push((selector, route_key));
                        }
                    }
                    if available.is_empty() {
                        continue;
                    }
                    let requested = available[0].0;
                    if let Some(stage) = &episode.source_read
                        && matches!(
                            journal.focused_stage(&stage.key(&episode.id)?).await?,
                            Some(FocusedStage::Outstanding | FocusedStage::Completed)
                        )
                    {
                        continue;
                    }
                    let population_stage = if requested == RecipeSelector::PageWaitTasks {
                        let stage = page_wait_tasks_stage_key(&episode.first.capture_ref(), &binding.source.target)?;
                        if populations.contains(&stage)
                            || matches!(
                                journal.focused_stage(&stage).await?,
                                Some(FocusedStage::Outstanding | FocusedStage::Completed)
                            )
                        {
                            continue;
                        }
                        Some(stage)
                    } else {
                        None
                    };
                    let Some(focus) = evidence.focus(outbox, target, episode, requested).await? else { continue };
                    // A trigger without its own original token cannot consume a valid sibling's population.
                    if let Some(stage) = population_stage {
                        populations.insert(stage);
                    }
                    let Some((_, route_key)) = available.iter().find(|(selector, _)| *selector == focus.selector())
                    else {
                        continue;
                    };
                    let Some(route) = routes.get(route_key).and_then(Option::as_ref) else { continue };
                    if let Some(candidate) = evidence.question(outbox, target, route, episode, binding, *focus).await?
                        && seen.insert(candidate.intent.episode_stage_key()?)
                    {
                        prepared.push(candidate);
                        break;
                    }
                    // One saved route owns each physical resource question. A completed database stage may still
                    // need a distinct, explicitly assigned shared owner; refusal never probes that owner's aliases.
                }
            }
            if cancellation.is_cancelled() {
                return Ok(prepared);
            }
            offset += 1;
            bindings.set_planning_position(cursor.clone(), offset);
            tokio::task::yield_now().await;
        }
        cursor = page.next_cursor;
        offset = 0;
        bindings.set_planning_position(cursor.clone(), 0);
        if cursor.is_none() {
            break;
        }
    }
    Ok(prepared)
}

fn possible_questions(episode: &DiagnosticEpisode) -> Vec<RecipeSelector> {
    if let Some(stage) = &episode.source_read {
        return if stage.needs_work() { vec![stage.focus.question.selector()] } else { vec![] };
    }
    match &episode.signal {
        DiagnosticSignal::LockWait { .. } => vec![RecipeSelector::FocusedBlocking],
        DiagnosticSignal::PageTaskWait { .. } => vec![RecipeSelector::FocusedPageTaskHolders],
        DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. } => {
            vec![RecipeSelector::CurrentRequestWork, RecipeSelector::PageWaitTasks]
        }
        DiagnosticSignal::RequestMemory { .. } => vec![RecipeSelector::FocusedRequestMemory],
        DiagnosticSignal::CompilationAdmission { .. } => vec![RecipeSelector::FocusedCompilationAdmission],
        DiagnosticSignal::PageWait { .. } if matches!(episode.report, Some(DiagnosticReport::PageWait { .. })) => {
            vec![RecipeSelector::FocusedPageWaitWriters, RecipeSelector::FocusedPageWait]
        }
        DiagnosticSignal::PageWait { .. } => vec![RecipeSelector::FocusedPageWait],
        _ => vec![],
    }
}

fn select_route(
    backend: &dyn FleetBackend,
    target: &FleetTarget,
    revision: &str,
    selector: RecipeSelector,
) -> Result<Option<SelectedRoute>> {
    if metadata(backend.route_revision(&target.connection))?.flatten().as_deref() != Some(revision) {
        return Ok(None);
    }
    let Some(recipes) = metadata(backend.telemetry_recipes(&target.connection))? else { return Ok(None) };
    if !recipe_catalog_is_valid(&recipes) {
        return Err(CollectionStop::InvalidPage.error());
    }
    if selector == RecipeSelector::PageWaitTasks
        && crate::diagnostics::long_running_question(&recipes, &target.subject) != Some(selector)
    {
        return Ok(None);
    }
    let Some(recipe) = recipes.into_iter().find(|recipe| {
        recipe.purpose == RecipePurpose::Investigation
            && recipe.selector == selector
            && recipe.subjects.iter().any(|subject| subject.accepts(&target.subject))
            && matches!(recipe.cost, RecipeCostQualification::Measured(_))
    }) else {
        return Ok(None);
    };
    let Some(component) = metadata(backend.qualification_component(&target.connection))?.flatten() else {
        return Ok(None);
    };
    if !qualification_component_is_valid(&component) {
        return Ok(None);
    }
    Ok(Some(SelectedRoute { recipe, component, revision: revision.into() }))
}

impl EvidencePlanner<'_> {
    async fn question(
        &self,
        outbox: &mut Outbox,
        target: &FleetTarget,
        route: &SelectedRoute,
        episode: &DiagnosticEpisode,
        binding: &RouteBinding,
        focus: DiagnosticFocus,
    ) -> Result<Option<PreparedInvestigation>> {
        let Some(budget) = binding.budget(target) else {
            return Ok(None);
        };
        let proposed = if let DiagnosticFocus::SourceRead { focus } = focus {
            let execution =
                self.execution_owners.get(&focus.key()?).ok_or_else(|| DeliveryProblem::Integrity.error())?;
            DiagnosticInvestigationIntent::new_source_read(
                episode,
                &route.recipe,
                *focus,
                execution.clone(),
                &route.component,
                budget,
            )
        } else if let Some(owner) = &binding.shared_owner {
            let Some((owner_source, owner_row)) =
                resolve_optional_evidence(outbox, &target.connection, owner, self.recovery, self.cancellation).await?
            else {
                return Ok(None);
            };
            if owner_source != binding.source {
                return Err(DeliveryProblem::Integrity.error());
            }
            DiagnosticInvestigationIntent::new_shared(
                episode,
                &route.recipe,
                focus,
                &route.component,
                budget,
                DiagnosticSharedOwner { source: &owner_source, row: &owner_row, reference: owner },
            )
        } else {
            DiagnosticInvestigationIntent::new(episode, &route.recipe, focus, &route.component, budget)
        };
        let Ok(intent) = proposed else {
            return Ok(None);
        };
        if !binding.matches_scope(&intent.qualification.scope) {
            return Ok(None);
        }
        match self.journal.focused_stage(&intent.episode_stage_key()?).await? {
            Some(FocusedStage::Outstanding | FocusedStage::Completed) => return Ok(None),
            Some(FocusedStage::Cancelled { key }) if key == intent.key()? => return Ok(None),
            _ => (),
        }
        if self.cancellation.is_cancelled() {
            return Ok(None);
        }
        let Some((cost_source, cost_row)) = resolve_optional_evidence(
            outbox,
            &target.connection,
            &intent.qualification.evidence,
            self.recovery,
            self.cancellation,
        )
        .await?
        else {
            return Ok(None);
        };
        if self.cancellation.is_cancelled()
            || !metadata(self.backend.verify_cost(&target.connection, &intent.qualification, &cost_source, &cost_row))?
                .unwrap_or(false)
            || metadata(self.backend.route_revision(&target.connection))?.flatten().as_deref()
                != Some(route.revision.as_str())
        {
            return Ok(None);
        }
        let job = FocusedJob::new(&intent, target, FOCUSED_COOLDOWN_SECONDS)?;
        Ok(Some(PreparedInvestigation { intent, job, route_revision: route.revision.clone() }))
    }
}

pub(super) fn metadata<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.detail.is_none() && matches!(error.code, ErrorCode::NotFound | ErrorCode::Unsupported) => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
