//! Prepare bounded questions from retained native evidence. This module never opens a source or grants admission.
use super::RouteBindings;
use crate::{
    capture::CollectionCancellation,
    delivery::recovery::RecoveryStore,
    diagnostics::resolve_evidence,
    fleet::{FleetBackend, FleetJournal, journal::FocusedStage, scheduler::FocusedJob},
    outbox::Outbox,
};
use culprit_core::{ComponentVersion, ErrorCode, Result, telemetry::*};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

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
    let mut routes = BTreeMap::new();
    let (mut cursor, mut offset) = bindings.planning_position();
    let mut scanned = 0;
    let mut questions = 0;
    let evidence = EvidencePlanner { backend, journal, recovery, cancellation };
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
            if episode.analysis.needs_work() && matches!(episode.signal, DiagnosticSignal::PageWait { .. }) {
                for target in targets {
                    if cancellation.is_cancelled() || questions == MAX_QUESTIONS_PER_PASS {
                        return Ok(prepared);
                    }
                    let Some(binding) = bindings.get(&target.id).filter(|binding| {
                        binding.matches_scope(&episode.scope)
                            && binding.source.observation.activity().is_some_and(|source| {
                                ObservationBounds::new(&episode.last_seen_at, &source.at).is_some()
                            })
                    }) else {
                        continue;
                    };
                    let Some(schedule) = target
                        .schedules
                        .iter()
                        .find(|schedule| matches!(schedule.collection, ScheduledCollection::Activity { .. }))
                    else {
                        continue;
                    };
                    if !routes.contains_key(&target.id) {
                        routes.insert(target.id.clone(), select_route(backend, target, &binding.revision)?);
                    }
                    let Some(route) = routes.get(&target.id).and_then(Option::as_ref) else { continue };
                    questions += 1;
                    if let Some(candidate) = evidence.question(outbox, target, &schedule.budget, route, episode).await?
                        && seen.insert(candidate.intent.episode_stage_key()?)
                    {
                        prepared.push(candidate);
                    }
                    // One explicit selected route owns this question in this pass; a refusal is not permission to
                    // probe alternative aliases. The episode cursor advances even when its proof is withheld.
                    break;
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

fn select_route(backend: &dyn FleetBackend, target: &FleetTarget, revision: &str) -> Result<Option<SelectedRoute>> {
    if metadata(backend.route_revision(&target.connection))?.flatten().as_deref() != Some(revision) {
        return Ok(None);
    }
    let Some(recipes) = metadata(backend.telemetry_recipes(&target.connection))? else { return Ok(None) };
    if !recipe_catalog_is_valid(&recipes) {
        return Err(CollectionStop::InvalidPage.error());
    }
    let Some(recipe) = recipes.into_iter().find(|recipe| {
        recipe.purpose == RecipePurpose::Investigation
            && recipe.selector == RecipeSelector::FocusedPageWait
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
        budget: &CollectionBudget,
        route: &SelectedRoute,
        episode: &DiagnosticEpisode,
    ) -> Result<Option<PreparedInvestigation>> {
        let Some((source, row)) =
            resolve_evidence(outbox, &target.connection, &episode.latest, self.recovery, self.cancellation).await?
        else {
            return Ok(None);
        };
        let TelemetryRecord::Activity(original) = &row.record else { return Err(DeliveryProblem::Integrity.error()) };
        let PageWaitFocusPlan::Ready(focus) = episode.plan_page_wait_focus(&source, original, &episode.latest)? else {
            return Ok(None);
        };
        let Ok(intent) = DiagnosticInvestigationIntent::new(episode, &route.recipe, *focus, &route.component, budget)
        else {
            return Ok(None);
        };
        match self.journal.focused_stage(&intent.episode_stage_key()?).await? {
            Some(FocusedStage::Outstanding | FocusedStage::Completed) => return Ok(None),
            Some(FocusedStage::Cancelled { key }) if key == intent.key()? => return Ok(None),
            _ => (),
        }
        if self.cancellation.is_cancelled() {
            return Ok(None);
        }
        let Some((cost_source, cost_row)) = resolve_evidence(
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
