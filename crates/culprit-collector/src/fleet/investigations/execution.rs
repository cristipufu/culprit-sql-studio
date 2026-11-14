//! Revalidate one admitted question before delegating to the shared native capture lifecycle.
use super::*;
use crate::{
    capture::CollectionCancellation,
    diagnostics::{resolve_evidence, set_analysis},
    fleet::{
        FleetBackend, FleetJournal,
        admission::AdmittedBackend,
        scheduler::{FocusRetry, FocusedJob, Outcome},
        storage::CaptureStorage,
        work,
    },
};

struct ReadyInvestigation {
    intent: DiagnosticInvestigationIntent,
    route_revision: String,
}

enum Revalidation {
    Ready(Box<ReadyInvestigation>),
    Withheld(DiagnosticAnalysisIssue),
    Cancelled,
}

impl Investigations {
    pub async fn execute(
        &self,
        backend: &dyn FleetBackend,
        admitted: &AdmittedBackend<'_>,
        journal: &FleetJournal,
        storage: &mut CaptureStorage<'_>,
        job: &FocusedJob,
        cancellation: &CollectionCancellation,
    ) -> Result<WorkResult> {
        let intent = journal.focused_intent(&job.key).await?.ok_or_else(|| DeliveryProblem::Integrity.error())?;
        if !job.matches_intent(&intent) {
            return Err(DeliveryProblem::Integrity.error());
        }
        let ready = match self.revalidate(backend, storage, job, &intent, cancellation).await? {
            Revalidation::Ready(ready) => ready,
            Revalidation::Withheld(issue) => {
                match resolve_evidence(
                    storage.outbox,
                    &job.connection,
                    &intent.focus.parent,
                    storage.recovery,
                    cancellation,
                )
                .await
                {
                    Ok(Some(_)) => {
                        set_analysis(
                            storage.outbox,
                            &intent.focus.parent,
                            &intent.episode_id,
                            DiagnosticAnalysisStatus::Deferred,
                            Some(issue.clone()),
                        )
                        .await?
                    }
                    Ok(None) => {
                        return failed(job, CollectionStop::Cancelled.error(), None, SourceWorkState::NotStarted);
                    }
                    Err(error) if evidence_unavailable(&error) => {}
                    Err(error) => return Err(error),
                }
                return withheld(job, &issue);
            }
            Revalidation::Cancelled => {
                return failed(job, CollectionStop::Cancelled.error(), None, SourceWorkState::NotStarted);
            }
        };
        set_analysis(
            storage.outbox,
            &intent.focus.parent,
            &intent.episode_id,
            DiagnosticAnalysisStatus::Collecting,
            None,
        )
        .await?;
        let request = CollectionRequest {
            subject: job.subject.clone(),
            selection: CollectionSelection::FocusedPageWait { focus: Box::new(ready.intent.focus.clone()) },
            budget: ready.intent.budget.clone(),
        };
        let manifest = match admitted
            .capture_routed(
                &job.connection,
                request.clone(),
                storage.outbox,
                cancellation,
                &ready.route_revision,
                &ready.intent,
            )
            .await
        {
            Ok(manifest) => manifest,
            Err(error) => {
                let result = failed(job, error, None, admitted.source_work())?;
                defer_native_retry(storage.outbox, &intent, &result).await?;
                return Ok(result);
            }
        };
        if manifest.request != request
            || manifest.description.as_ref().is_some_and(|source| !source.matches_request(&request))
        {
            return Err(CollectionStop::InvalidPage.error());
        }
        let result = if manifest.status == CollectionStatus::Complete {
            let input = storage
                .outbox
                .diagnostic_input(&manifest.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
                .await?;
            if input.parent.capture != manifest.capture
                || manifest.description.as_ref() != Some(&input.source)
                || !super::queue::result_matches_intent(&input.source, &ready.intent)?
            {
                return Err(DeliveryProblem::Integrity.error());
            }
            Some(input.parent)
        } else {
            None
        };
        let attempt = Some(manifest.capture.clone());
        let mut completed = work::completed_for(job.into(), manifest, None, false)?;
        completed.outcome = match result {
            Some(result) => Outcome::FocusedComplete { result },
            None => retry(completed.outcome, attempt, admitted.source_work())?,
        };
        if matches!(completed.outcome, Outcome::FocusedRetry { .. }) {
            defer_native_retry(storage.outbox, &intent, &completed).await?;
        }
        Ok(completed)
    }

    async fn revalidate(
        &self,
        backend: &dyn FleetBackend,
        storage: &mut CaptureStorage<'_>,
        job: &FocusedJob,
        intent: &DiagnosticInvestigationIntent,
        cancellation: &CollectionCancellation,
    ) -> Result<Revalidation> {
        let Some(target) = self.targets.iter().find(|target| target.id == job.target) else {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::PolicyChanged));
        };
        let Some(binding) = self.bindings.get(&target.id) else {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::SourceUnavailable {
                code: None,
                stopped: None,
            }));
        };
        if !binding.matches_scope(&intent.qualification.scope) {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::SourceChanged));
        }
        let Some(revision) = planning::metadata(backend.route_revision(&target.connection))?.flatten() else {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::SourceUnavailable {
                code: None,
                stopped: None,
            }));
        };
        if revision != binding.revision {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::SourceChanged));
        }
        let Some(catalog) = planning::metadata(backend.telemetry_recipes(&target.connection))? else {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::Unsupported));
        };
        if !recipe_catalog_is_valid(&catalog) {
            return Err(CollectionStop::InvalidPage.error());
        }
        let Some(recipe) =
            catalog.iter().find(|recipe| recipe.id == intent.recipe_id && recipe.version == intent.recipe_version)
        else {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::Unsupported));
        };
        let Some(component) = planning::metadata(backend.qualification_component(&target.connection))? else {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::SourceUnavailable {
                code: None,
                stopped: None,
            }));
        };
        let Some(component) = component else {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::CostUnqualified));
        };
        let Some((source, original)) =
            resolve_evidence(storage.outbox, &job.connection, &intent.focus.parent, storage.recovery, cancellation)
                .await?
        else {
            return Ok(Revalidation::Cancelled);
        };
        let TelemetryRecord::Activity(original) = &original.record else {
            return Err(CollectionStop::InvalidPage.error());
        };
        if !intent.focus.matches_parent(&source, original, &intent.focus.parent) {
            return Err(CollectionStop::InvalidPage.error());
        }
        storage.outbox.pin_investigation_parent(&job.key, &intent.focus).await?;
        let current = storage
            .outbox
            .diagnostic_input(&intent.focus.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
            .await?;
        let episode = current
            .checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.episodes.iter().find(|episode| episode.id == intent.episode_id));
        let Some(episode) = episode else {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::NativeContextGone));
        };
        if binding
            .source
            .observation
            .activity()
            .is_none_or(|frame| ObservationBounds::new(&episode.last_seen_at, &frame.at).is_none())
        {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::SourceChanged));
        }
        let caller_budget = target.schedules.iter().find_map(|schedule| {
            matches!(schedule.collection, ScheduledCollection::Activity { .. }).then_some(&schedule.budget)
        });
        if caller_budget.is_none_or(|budget| !intent.matches_admission(episode, recipe, &component, budget)) {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::PolicyChanged));
        }
        let cost = match resolve_evidence(
            storage.outbox,
            &job.connection,
            &intent.qualification.evidence,
            storage.recovery,
            cancellation,
        )
        .await
        {
            Ok(cost) => cost,
            Err(error) if evidence_unavailable(&error) => {
                return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::EvidenceUnavailable));
            }
            Err(error) => return Err(error),
        };
        let Some((cost_source, artifact)) = cost else { return Ok(Revalidation::Cancelled) };
        if !backend.verify_cost(&job.connection, &intent.qualification, &cost_source, &artifact)? {
            return Ok(Revalidation::Withheld(DiagnosticAnalysisIssue::CostUnqualified));
        }
        Ok(Revalidation::Ready(Box::new(ReadyInvestigation {
            intent: intent.clone(),
            route_revision: binding.revision.clone(),
        })))
    }
}

async fn defer_native_retry(
    outbox: &mut crate::outbox::Outbox,
    intent: &DiagnosticInvestigationIntent,
    result: &WorkResult,
) -> Result<()> {
    let CollectionStatus::Incomplete(stopped) = result.event.status else {
        return Err(CollectionStop::InvalidPage.error());
    };
    let issue = if result.pause_all {
        DiagnosticAnalysisIssue::Capacity
    } else {
        match stopped {
            CollectionStop::SourceChanged => DiagnosticAnalysisIssue::SourceChanged,
            CollectionStop::Cancelled => DiagnosticAnalysisIssue::Interrupted,
            CollectionStop::PageLimit
            | CollectionStop::QueryLimit
            | CollectionStop::ByteLimit
            | CollectionStop::RowLimit => DiagnosticAnalysisIssue::BudgetExhausted,
            _ => DiagnosticAnalysisIssue::SourceUnavailable { code: result.event.error_code, stopped: Some(stopped) },
        }
    };
    set_analysis(outbox, &intent.focus.parent, &intent.episode_id, DiagnosticAnalysisStatus::Deferred, Some(issue))
        .await
}

pub(super) fn evidence_unavailable(error: &culprit_core::Error) -> bool {
    matches!(
        error.detail,
        Some(culprit_core::ErrorDetail::Outbox(OutboxProblem::MissingCapture))
            | Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::NotReady))
    )
}

fn withheld(job: &FocusedJob, issue: &DiagnosticAnalysisIssue) -> Result<WorkResult> {
    let stopped = match issue {
        DiagnosticAnalysisIssue::SourceChanged | DiagnosticAnalysisIssue::NativeContextGone => {
            CollectionStop::SourceChanged
        }
        DiagnosticAnalysisIssue::BudgetExhausted => CollectionStop::QueryLimit,
        _ => CollectionStop::SourceNotReady,
    };
    failed(job, stopped.error(), None, SourceWorkState::NotStarted)
}

fn failed(
    job: &FocusedJob,
    error: culprit_core::Error,
    attempt: Option<CaptureIdentity>,
    source_work: SourceWorkState,
) -> Result<WorkResult> {
    let mut result = work::failure_for(job.into(), FleetWork::Capture, error, None, Some(source_work))?;
    result.outcome = retry(result.outcome, attempt, source_work)?;
    Ok(result)
}

fn retry(outcome: Outcome, attempt: Option<CaptureIdentity>, source_work: SourceWorkState) -> Result<Outcome> {
    let reason = match outcome {
        Outcome::More => FocusRetry::More,
        Outcome::Pressure => FocusRetry::Pressure,
        Outcome::Failed => FocusRetry::Failed,
        _ => return Err(CollectionStop::InvalidPage.error()),
    };
    Ok(Outcome::FocusedRetry { reason, attempt, source_work })
}
