//! Parent evidence is pinned before canonical enrollment; journal acknowledgement precedes pin release.
use super::*;
use crate::{
    capture::CollectionCancellation,
    diagnostics::{resolve_evidence, set_analysis},
    fleet::{
        FleetBackend, FleetJournal,
        scheduler::{FocusEnrollment, FocusStatus, Scheduler, state_failed},
        storage::CaptureStorage,
    },
    outbox::DiagnosticInputState,
};
use std::{collections::BTreeSet, time::Duration};

const MAX_MAINTENANCE_JOBS: usize = 8;
const RECEIPT_SCAN_CADENCE: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct Maintenance {
    after: Option<String>,
    receipts: Option<BTreeSet<String>>,
    next_receipt_scan: Duration,
}

struct QueueOwners<'a, 'storage> {
    backend: &'a dyn FleetBackend,
    journal: &'a FleetJournal,
    storage: &'a mut CaptureStorage<'storage>,
    scheduler: &'a mut Scheduler,
}

impl Investigations {
    pub async fn prepare(
        &self,
        backend: &dyn FleetBackend,
        journal: &FleetJournal,
        storage: &mut CaptureStorage<'_>,
        scheduler: &mut Scheduler,
        started: tokio::time::Instant,
        cancellation: &CollectionCancellation,
    ) -> Result<bool> {
        let prepared = planning::prepare(
            backend,
            journal,
            storage.outbox,
            storage.recovery,
            &self.targets,
            &self.bindings,
            cancellation,
        )
        .await?;
        let mut enqueued = false;
        for candidate in prepared {
            if cancellation.is_cancelled() {
                break;
            }
            let key = candidate.intent.key()?;
            if self
                .bindings
                .get(&candidate.job.target)
                .is_none_or(|binding| binding.revision != candidate.route_revision)
            {
                continue;
            }
            storage.outbox.pin_investigation_parent(&key, &candidate.intent.focus).await?;
            match scheduler.enqueue_focus(candidate.job, started.elapsed(), chrono::Utc::now())? {
                FocusEnrollment::Enqueued(update) => {
                    let clock = journal.enqueue_focused(candidate.intent, *update).await?;
                    scheduler.persisted(clock)?;
                    enqueued = true;
                }
                FocusEnrollment::Existing { key: existing } if existing == key => {}
                FocusEnrollment::Capacity => {
                    set_analysis(
                        storage.outbox,
                        &candidate.intent.focus.parent,
                        &candidate.intent.episode_id,
                        DiagnosticAnalysisStatus::Deferred,
                        Some(DiagnosticAnalysisIssue::Capacity),
                    )
                    .await?;
                    storage.outbox.release_investigation_parent(&key).await?;
                }
                FocusEnrollment::Existing { .. } => {
                    storage.outbox.release_investigation_parent(&key).await?;
                }
            }
        }
        Ok(enqueued)
    }

    /// Use the complete bounded canonical inventory; a crash between pin and enrollment leaves only a safe orphan.
    pub async fn reconcile(
        &mut self,
        backend: &dyn FleetBackend,
        journal: &FleetJournal,
        storage: &mut CaptureStorage<'_>,
        scheduler: &mut Scheduler,
        started: tokio::time::Instant,
        cancellation: &CollectionCancellation,
    ) -> Result<()> {
        let intents = journal.focused_intents().await?;
        let keys = intents.iter().map(DiagnosticInvestigationIntent::key).collect::<Result<BTreeSet<_>>>()?;
        if scheduler.focused_keys().into_iter().collect::<BTreeSet<_>>() != keys {
            return Err(state_failed());
        }
        for key in storage.outbox.investigation_parent_keys().await? {
            if !keys.contains(&key) {
                storage.outbox.release_investigation_parent(&key).await?;
            }
        }
        // Startup reconciles local owners only. Pending parents are authenticated/pinned by selected revalidation;
        // no cold queue may perform hundreds of recovery reads before the first fresh native baseline.
        if cancellation.is_cancelled() || scheduler.wait_allowed(started.elapsed(), |_| false) < planning::PLANNING_TIME
        {
            return Ok(());
        }
        let stopped = CollectionCancellation::default();
        let finished = CollectionCancellation::default();
        let work = async {
            let result = self
                .maintain(&mut QueueOwners { backend, journal, storage, scheduler }, started, &stopped, intents)
                .await;
            finished.cancel();
            result
        };
        let relay = async {
            tokio::select! { biased;
                _ = cancellation.cancelled() => stopped.cancel(),
                _ = tokio::time::sleep(planning::PLANNING_TIME) => stopped.cancel(),
                _ = finished.cancelled() => {},
            }
        };
        // Read cancellation is cooperative; an already-started cache/journal mutation retains its owner.
        let (result, ()) = tokio::join!(work, relay);
        result
    }

    async fn maintain(
        &mut self,
        owners: &mut QueueOwners<'_, '_>,
        started: tokio::time::Instant,
        cancellation: &CollectionCancellation,
        mut intents: Vec<DiagnosticInvestigationIntent>,
    ) -> Result<()> {
        if let Some(after) = &self.maintenance.after {
            let split = intents.partition_point(|intent| intent.key().is_ok_and(|key| key <= *after));
            intents.rotate_left(split);
        }
        for intent in intents.into_iter().take(MAX_MAINTENANCE_JOBS) {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            let key = intent.key()?;
            // A slow/missing artifact cannot monopolize every later metadata pass.
            self.maintenance.after = Some(key.clone());
            let state = owners.scheduler.focused_state(&key).ok_or_else(state_failed)?;
            if !state.job.matches_intent(&intent) {
                return Err(state_failed());
            }
            if let FocusStatus::Completed { result } = &state.status {
                if self.result_consumed(owners.storage, &intent, result, cancellation).await? {
                    let update = owners.scheduler.retire_focused(&key, started.elapsed(), chrono::Utc::now())?;
                    owners.scheduler.persisted(owners.journal.retire_focused(update).await?)?;
                    owners.storage.outbox.release_investigation_parent(&key).await?;
                }
            } else if state.status == FocusStatus::Pending
                && matches!(state.source_work, SourceWorkState::NotStarted | SourceWorkState::Quiescent)
                && self.bindings.get(&state.job.target).is_some()
            {
                self.cancel_stale(owners, started, cancellation, &intent, &state.job).await?;
            }
        }
        if cancellation.is_cancelled() {
            return Ok(());
        }
        for family in owners.scheduler.focused_family_keys() {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            if let Some(update) =
                owners.scheduler.retire_focused_family(&family, started.elapsed(), chrono::Utc::now())?
            {
                let clock = owners.journal.update_scheduler(update).await?;
                owners.scheduler.persisted(clock)?;
            }
        }
        self.prune_receipts(owners.journal, owners.storage.outbox, started.elapsed(), cancellation).await
    }

    async fn cancel_stale(
        &self,
        owners: &mut QueueOwners<'_, '_>,
        started: tokio::time::Instant,
        cancellation: &CollectionCancellation,
        intent: &DiagnosticInvestigationIntent,
        job: &crate::fleet::scheduler::FocusedJob,
    ) -> Result<()> {
        let evidence = match resolve_evidence(
            owners.storage.outbox,
            &job.connection,
            &intent.focus.parent,
            owners.storage.recovery,
            cancellation,
        )
        .await
        {
            Ok(evidence) => evidence,
            Err(error) if super::execution::evidence_unavailable(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        let Some((source, row)) = evidence else { return Ok(()) };
        let TelemetryRecord::Activity(original) = &row.record else { return Err(DeliveryProblem::Integrity.error()) };
        if !intent.focus.matches_parent(&source, original, &intent.focus.parent) {
            return Err(DeliveryProblem::Integrity.error());
        }
        let current = owners
            .storage
            .outbox
            .diagnostic_input(&intent.focus.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
            .await?;
        let Some(checkpoint) = current.checkpoint else { return Ok(()) };
        let episode = checkpoint.episodes.iter().find(|episode| episode.id == intent.episode_id);
        let decision = match episode {
            None => Some((DiagnosticAnalysisIssue::PolicyChanged, None)),
            Some(episode) if !episode.matches_focus(&intent.focus) => return Err(DeliveryProblem::Integrity.error()),
            Some(episode) if !episode.analysis.needs_work() => Some((DiagnosticAnalysisIssue::PolicyChanged, None)),
            Some(episode) => self
                .metadata_issue(owners.backend, job, intent, episode)?
                .map(|issue| (issue, Some(DiagnosticAnalysisStatus::Deferred))),
        };
        let Some((issue, status)) = decision else { return Ok(()) };
        if cancellation.is_cancelled() {
            return Ok(());
        }
        if let Some(status) = status {
            set_analysis(owners.storage.outbox, &intent.focus.parent, &intent.episode_id, status, Some(issue.clone()))
                .await?;
        }
        let update = owners.scheduler.cancel_focused(&job.key, started.elapsed(), chrono::Utc::now())?;
        owners.scheduler.persisted(owners.journal.cancel_focused(update, issue).await?)?;
        owners.storage.outbox.release_investigation_parent(&job.key).await
    }

    fn metadata_issue(
        &self,
        backend: &dyn FleetBackend,
        job: &crate::fleet::scheduler::FocusedJob,
        intent: &DiagnosticInvestigationIntent,
        episode: &DiagnosticEpisode,
    ) -> Result<Option<DiagnosticAnalysisIssue>> {
        let Some(target) = self.targets.iter().find(|target| target.id == job.target) else {
            return Ok(Some(DiagnosticAnalysisIssue::PolicyChanged));
        };
        let binding = self.bindings.get(&job.target).ok_or_else(state_failed)?;
        if !binding.matches_scope(&intent.qualification.scope) {
            return Ok(Some(DiagnosticAnalysisIssue::SourceChanged));
        }
        let Some(revision) = planning::metadata(backend.route_revision(&target.connection))?.flatten() else {
            return Ok(Some(DiagnosticAnalysisIssue::SourceUnavailable { code: None, stopped: None }));
        };
        if revision != binding.revision {
            return Ok(Some(DiagnosticAnalysisIssue::SourceChanged));
        }
        let Some(catalog) = planning::metadata(backend.telemetry_recipes(&target.connection))? else {
            return Ok(Some(DiagnosticAnalysisIssue::Unsupported));
        };
        if !recipe_catalog_is_valid(&catalog) {
            return Err(CollectionStop::InvalidPage.error());
        }
        let Some(recipe) =
            catalog.iter().find(|recipe| recipe.id == intent.recipe_id && recipe.version == intent.recipe_version)
        else {
            return Ok(Some(DiagnosticAnalysisIssue::Unsupported));
        };
        let Some(component) = planning::metadata(backend.qualification_component(&target.connection))? else {
            return Ok(Some(DiagnosticAnalysisIssue::SourceUnavailable { code: None, stopped: None }));
        };
        let Some(component) = component else {
            return Ok(Some(DiagnosticAnalysisIssue::CostUnqualified));
        };
        let budget = target.schedules.iter().find_map(|schedule| {
            matches!(schedule.collection, ScheduledCollection::Activity { .. }).then_some(&schedule.budget)
        });
        Ok(budget
            .is_none_or(|budget| !intent.matches_admission(episode, recipe, &component, budget))
            .then_some(DiagnosticAnalysisIssue::PolicyChanged))
    }

    async fn prune_receipts(
        &mut self,
        journal: &FleetJournal,
        outbox: &mut crate::outbox::Outbox,
        now: Duration,
        cancellation: &CollectionCancellation,
    ) -> Result<()> {
        let receipts = journal.focused_receipt_episode_ids().await?;
        if receipts.is_empty() {
            self.maintenance.receipts = Some(receipts);
            return Ok(());
        }
        if self.maintenance.receipts.as_ref() == Some(&receipts) && now < self.maintenance.next_receipt_scan {
            return Ok(());
        }
        let mut retained = BTreeSet::new();
        let mut cursor = None;
        loop {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            let page = outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, cursor.as_deref(), 1).await?;
            for checkpoint in page.checkpoints {
                retained.extend(
                    checkpoint.episodes.into_iter().map(|episode| episode.id).filter(|id| receipts.contains(id)),
                );
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        if !cancellation.is_cancelled() {
            journal.prune_focused_receipts(&retained).await?;
            self.maintenance.receipts = Some(retained);
            self.maintenance.next_receipt_scan = now.saturating_add(RECEIPT_SCAN_CADENCE);
        }
        Ok(())
    }

    async fn result_consumed(
        &self,
        storage: &mut CaptureStorage<'_>,
        intent: &DiagnosticInvestigationIntent,
        result: &DiagnosticCaptureRef,
        cancellation: &CollectionCancellation,
    ) -> Result<bool> {
        if cancellation.is_cancelled() {
            return Ok(false);
        }
        let locally_consumed =
            match storage.outbox.diagnostic_input_state(result, DIAGNOSTIC_CONTROLLER_VERSION).await? {
                DiagnosticInputState::Pending => return Ok(false),
                state @ (DiagnosticInputState::Consumed | DiagnosticInputState::EvidenceOnly) => {
                    match storage
                        .outbox
                        .diagnostic_input(&result.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
                        .await
                    {
                        Ok(input) => {
                            if input.parent != *result || !result_matches_intent(&input.source, intent)? {
                                return Err(DeliveryProblem::Integrity.error());
                            }
                            state == DiagnosticInputState::Consumed
                        }
                        // Archive acknowledgement may evict raw bytes before compacting their consumption row.
                        // Only actual absence may use the configured archive; a corrupt local artifact still fails.
                        Err(error)
                            if storage.recovery.is_some()
                                && error.detail
                                    == Some(culprit_core::ErrorDetail::Outbox(OutboxProblem::MissingCapture)) =>
                        {
                            false
                        }
                        Err(error) => return Err(error),
                    }
                }
                DiagnosticInputState::Missing => false,
            };
        let Some(recovery) = storage.recovery else { return Ok(locally_consumed) };
        if storage.outbox.recovery_store_id() != Some(recovery.id()) {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        let recovered = tokio::select! { biased;
            _ = cancellation.cancelled() => return Ok(false),
            result = tokio::time::timeout(Duration::from_secs(30), async {
                let manifest = recovery.capture_manifest(&result.capture).await?;
                if crate::delivery::diagnostic_recovery::capture_reference(&manifest)? != *result
                    || !result_matches_intent(&manifest.source, intent)?
                {
                    return Err(DeliveryProblem::Integrity.error());
                }
                let recovered = crate::delivery::diagnostic_recovery::load_diagnostic_input(recovery, &manifest.range_id).await?;
                if recovered.range().manifest_digest() != manifest.digest()?
                    || crate::delivery::diagnostic_recovery::capture_reference(recovered.range().manifest())? != *result
                    || !result_matches_intent(&recovered.range().manifest().source, intent)?
                {
                    return Err(DeliveryProblem::Integrity.error());
                }
                recovery.diagnostic_input_pending(result, DIAGNOSTIC_CONTROLLER_VERSION).await.map(|pending| !pending)
            }) => result.map_err(|_| DeliveryProblem::Storage.error())?,
        };
        match recovered {
            Err(error) if super::execution::evidence_unavailable(&error) => Ok(false),
            recovered => recovered,
        }
    }
}

#[cfg(test)]
mod tests;

pub(super) fn result_matches_intent(
    source: &CollectionDescription,
    intent: &DiagnosticInvestigationIntent,
) -> Result<bool> {
    let SourceObservation::FocusedPageWait { result, .. } = &source.observation else {
        return Err(DeliveryProblem::Integrity.error());
    };
    let expected_key = intent.key()?;
    Ok(result.is_valid()
        && result.focus == intent.focus
        && result.lineage.as_ref().is_some_and(|lineage| {
            lineage.intent_key == expected_key
                && lineage
                    .reconstruct(&result.focus)
                    .is_ok_and(|reconstructed| reconstructed.key().is_ok_and(|key| key == expected_key))
        }))
}
