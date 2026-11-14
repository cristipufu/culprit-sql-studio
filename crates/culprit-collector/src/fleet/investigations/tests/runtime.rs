use super::super::Investigations;
use super::support::Harness;
use crate::{
    capture::CollectionCancellation,
    diagnostics::{DiagnosticPolicies, analyze_pending, drain_pending, set_analysis},
    fleet::{
        admission::AdmittedBackend,
        journal::{FocusedSettlement, FocusedStage},
        scheduler::{FocusStatus, FocusedJob, Outcome, Scheduler, Work},
        storage::CaptureStorage,
        work::{self, WorkResult},
    },
    outbox::{DiagnosticInputState, Outbox},
};
use culprit_core::telemetry::*;
use std::{path::Path, sync::atomic::Ordering};
use tokio::time::Instant;

mod provenance;

struct Runtime {
    fixture: Harness,
    investigations: Investigations,
    scheduler: Scheduler,
    started: Instant,
}
impl Runtime {
    async fn new() -> Self {
        let fixture = Harness::new(1).await;
        Self::from_fixture(fixture).await
    }

    async fn from_fixture(fixture: Harness) -> Self {
        let mut scheduler = Scheduler::new(&fixture.inventory, "one").unwrap();
        let started = Instant::now();
        let state = scheduler.initial_state(started.elapsed(), chrono::Utc::now()).unwrap();
        scheduler.persisted(fixture.journal.initialize_scheduler(state).await.unwrap()).unwrap();
        let investigations = Investigations::new(&fixture.inventory, "one");
        let mut runtime = Self { fixture, investigations, scheduler, started };
        runtime.baseline().await;
        runtime
    }

    async fn baseline(&mut self) {
        let Work::Scheduled(job) = self.scheduler.take_allowed(self.started.elapsed(), |_| false).unwrap().unwrap()
        else {
            panic!("the initial periodic baseline runs before any targeted work")
        };
        self.persist_admission().await;
        self.investigations.starting_baseline(&job);
        let admitted = AdmittedBackend::new(&self.fixture.backend, Instant::now());
        let result = work::execute(
            &admitted,
            &self.fixture.journal,
            &mut CaptureStorage::local(&mut self.fixture.outbox),
            &job,
            &CollectionCancellation::default(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(result.event.status, CollectionStatus::Complete);
        self.investigations.completed_baseline(&job, &result, admitted.used_route_revision()).unwrap();
        self.finish(result.outcome).await;
        let policies = self.policies();
        drain_pending(&mut self.fixture.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    }

    fn policies(&self) -> DiagnosticPolicies {
        DiagnosticPolicies::from_inventory(&self.fixture.inventory, "one")
            .unwrap()
            .with_recipes("saved-source", vec![self.fixture.backend.recipe.clone()])
            .unwrap()
    }

    async fn complete_empty_baseline(&mut self) {
        let activity = &mut self.fixture.backend.activity;
        let SourceObservation::Activity(frame) = &mut activity.source.observation else { panic!() };
        let at = chrono::DateTime::parse_from_rfc3339(&frame.at).unwrap() + chrono::Duration::seconds(1);
        frame.at = at.to_rfc3339();
        frame.observed_from = frame.at.clone();
        frame.observed_until = (at + chrono::Duration::seconds(1)).to_rfc3339();
        frame.native_rows = 0;
        frame.observations = 0;
        frame.coverage = StateSourceCoverage::collected(activity.source.source.clone(), 0);
        activity.records.clear();
        self.started -= self.scheduler.wait_allowed(self.started.elapsed(), |_| false);
        self.baseline().await;
    }

    async fn enroll(&mut self) -> bool {
        self.investigations
            .prepare(
                &self.fixture.backend,
                &self.fixture.journal,
                &mut CaptureStorage::local(&mut self.fixture.outbox),
                &mut self.scheduler,
                self.started,
                &CollectionCancellation::default(),
            )
            .await
            .unwrap()
    }

    async fn take_focus(&mut self) -> FocusedJob {
        // Only advance scheduler control time. Native capture uses its real admission instant and deadlines.
        self.started -=
            self.scheduler.wait_allowed(self.started.elapsed(), |job| self.investigations.route_available(job));
        let now = self.started.elapsed();
        assert!(
            self.scheduler.take_allowed(now, |_| false).unwrap().is_none(),
            "targeted work requires the selected live route"
        );
        let Work::Focused(job) =
            self.scheduler.take_allowed(now, |job| self.investigations.route_available(job)).unwrap().unwrap()
        else {
            panic!("the periodic baseline is not due again")
        };
        self.persist_admission().await;
        job
    }

    async fn persist_admission(&mut self) {
        let update = self.scheduler.admission_checkpoint(self.started.elapsed(), chrono::Utc::now()).unwrap();
        self.scheduler.persisted(self.fixture.journal.update_scheduler(update).await.unwrap()).unwrap();
    }

    async fn execute(&mut self, job: &FocusedJob) -> WorkResult {
        let admitted = AdmittedBackend::new(&self.fixture.backend, Instant::now());
        self.investigations
            .execute(
                &self.fixture.backend,
                &admitted,
                &self.fixture.journal,
                &mut CaptureStorage::local(&mut self.fixture.outbox),
                job,
                &CollectionCancellation::default(),
            )
            .await
            .unwrap()
    }

    async fn finish(&mut self, outcome: Outcome) {
        self.scheduler.finish(self.started.elapsed(), outcome).unwrap();
        let update = self.scheduler.completion_checkpoint(self.started.elapsed(), chrono::Utc::now()).unwrap();
        self.scheduler.persisted(self.fixture.journal.update_scheduler(update).await.unwrap()).unwrap();
    }

    async fn reconcile(&mut self) {
        self.investigations
            .reconcile(
                &self.fixture.backend,
                &self.fixture.journal,
                &mut CaptureStorage::local(&mut self.fixture.outbox),
                &mut self.scheduler,
                self.started,
                &CollectionCancellation::default(),
            )
            .await
            .unwrap();
    }

    async fn revisions(&self, parent: &CaptureIdentity) -> Vec<DiagnosticRevision> {
        let status = self.fixture.outbox.status(100, None).await.unwrap();
        let captures: Vec<_> = status.entries.iter().filter(|entry| {
            matches!(&entry.request.selection, CollectionSelection::Diagnostics { parent: input, .. } if input.capture == *parent)
        }).collect();
        assert_eq!(captures.len(), 1, "one input owns one offline transition");
        let directory = self.fixture._directory.path().join("offline-revision");
        replay(&self.fixture._directory.path().join("outbox"), &captures[0].capture, &directory)
            .await
            .into_iter()
            .filter_map(|row| match row.record {
                TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::DiagnosticRevision(revision),
                    ..
                }) => Some(*revision),
                TelemetryRecord::DiagnosticCheckpoint(_) => None,
                _ => panic!("offline history must reference original native evidence, without duplicating its payload"),
            })
            .collect()
    }

    fn native_calls(&self) -> [usize; 5] {
        let calls = &self.fixture.backend.calls;
        [&calls.captured, &calls.opened, &calls.observed, &calls.verified, &calls.closed]
            .map(|count| count.load(Ordering::SeqCst))
    }
}

async fn replay(path: &Path, capture: &CaptureIdentity, destination: &Path) -> Vec<ExportedRecord> {
    let replayed = Outbox::replay(path, &capture.observation_id, destination).await.unwrap();
    assert_eq!(replayed.capture, *capture);
    let manifest: CollectionManifest =
        serde_json::from_slice(&std::fs::read(destination.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    manifest
        .files
        .iter()
        .flat_map(|file| {
            std::fs::read_to_string(destination.join(&file.file))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test]
async fn admitted_missing_focus_keeps_lineage_and_original_after_terminal_retirement_and_restart() {
    // The receipt and native frame are synthetic; this exercises ownership, not native-cost qualification.
    let mut runtime = Runtime::new().await;
    assert_eq!(runtime.native_calls(), [1; 5]);
    assert!(runtime.enroll().await);
    let intents = runtime.fixture.journal.focused_intents().await.unwrap();
    assert_eq!(intents.len(), 1);
    let intent = intents[0].clone();
    let key = intent.key().unwrap();
    let original = runtime.fixture.outbox.diagnostic_evidence(&intent.focus.parent).await.unwrap();
    assert_eq!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap(), vec![key.clone()]);

    let job = runtime.take_focus().await;
    assert_eq!(job.key, key);
    let result = runtime.execute(&job).await;
    assert_eq!(runtime.native_calls(), [2; 5], "both real capture owners must close their source");
    assert_eq!(result.event.status, CollectionStatus::Complete);
    let SourceObservation::FocusedPageWait { snapshot, result: focused } = &result.source.as_ref().unwrap().observation
    else {
        panic!()
    };
    assert_eq!(snapshot.records, 0);
    assert_eq!(focused.focus, intent.focus);
    assert_eq!(focused.request, PageWaitRequestState::Missing);
    assert_eq!(focused.wait, PageWaitRelation::Unqualified);
    let Outcome::FocusedComplete { result: capture } = &result.outcome else {
        panic!("a sealed native result is distinct from completed analysis")
    };
    let capture = capture.clone();
    let manifest = runtime.fixture.outbox.verify(&capture.capture.observation_id).await.unwrap();
    assert_eq!(manifest.investigation_intent().unwrap(), Some(intent.clone()));
    let input = runtime
        .fixture
        .outbox
        .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    assert_eq!(input.parent, capture);
    assert!(input.rows.is_empty());
    assert_eq!(input.source, *result.source.as_ref().unwrap());
    runtime.finish(result.outcome).await;

    runtime.reconcile().await;
    assert!(
        runtime.fixture.journal.focused_intent(&key).await.unwrap().is_some(),
        "unconsumed focused results retain canonical intent"
    );
    assert!(matches!(runtime.scheduler.focused_state(&key).unwrap().status, FocusStatus::Completed { .. }));
    assert_eq!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap(), vec![key.clone()]);

    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    assert_eq!(runtime.native_calls(), [2; 5], "the same worker never reopens the source");
    assert_eq!(
        runtime.fixture.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Consumed
    );
    let revisions = runtime.revisions(&capture.capture).await;
    assert_eq!(revisions.len(), 1);
    let episode = &revisions[0].episode;
    assert_eq!(episode.id, intent.episode_id);
    assert_eq!(episode.latest, intent.focus.parent);
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::NativeContextGone));
    assert!(episode.report.is_none(), "a missing participant never supplies a mapped target or causal report");
    assert_eq!(runtime.fixture.outbox.diagnostic_evidence(&intent.focus.parent).await.unwrap(), original);

    runtime.reconcile().await;
    assert!(runtime.fixture.journal.focused_intent(&key).await.unwrap().is_none());
    assert!(runtime.scheduler.focused_keys().is_empty());
    assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
    assert!(
        runtime.fixture.journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap().is_none(),
        "terminal episodes retire current replay receipts after immutable result consumption"
    );

    let Harness { _directory: directory, inventory, outbox, journal, backend, .. } = runtime.fixture;
    drop(outbox);
    drop(journal);
    let mut reopened = Outbox::open(&directory.path().join("outbox")).unwrap();
    let recovered = reopened.recover().await.unwrap();
    assert_eq!((recovered.interrupted, recovered.quarantined, recovered.source_debts), (0, 0, 0));
    assert_eq!(reopened.diagnostic_evidence(&intent.focus.parent).await.unwrap(), original);
    let replayed = replay(
        &directory.path().join("outbox"),
        &intent.focus.parent.capture,
        &directory.path().join("original-after-restart"),
    )
    .await;
    assert_eq!(replayed, vec![original.1]);
    let restored =
        reopened.diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    assert!(matches!(restored.source.observation, SourceObservation::FocusedPageWait { .. }));
    assert_eq!(
        reopened.verify(&capture.capture.observation_id).await.unwrap().investigation_intent().unwrap(),
        Some(intent.clone())
    );
    let journal = crate::fleet::FleetJournal::open(&directory.path().join("fleet"), &inventory, "one").await.unwrap();
    assert!(journal.focused_intents().await.unwrap().is_empty());
    assert!(journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap().is_none());
    assert_eq!(backend.calls.captured.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn complete_scope_recovery_preserves_one_owed_original_revalidation() {
    let mut runtime = Runtime::new().await;
    runtime.complete_empty_baseline().await;
    assert_eq!(runtime.native_calls(), [2; 5]);
    let page = runtime.fixture.outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap();
    let episodes: Vec<_> = page.checkpoints.into_iter().flat_map(|checkpoint| checkpoint.episodes).collect();
    assert_eq!(episodes.len(), 1);
    let recovered = &episodes[0];
    assert_eq!(recovered.lifecycle, DiagnosticLifecycle::Resolved);
    assert!(recovered.analysis.needs_work(), "symptom recovery does not settle the retained analysis question");
    assert!(matches!(recovered.recovery, Some(DiagnosticRecoveryProof::CompleteScope { .. })));
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    let intent = runtime.fixture.journal.focused_intent(&job.key).await.unwrap().unwrap();
    assert_eq!(intent.episode_id, recovered.id);
    assert_eq!(intent.focus.parent, recovered.latest);
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: captured } = &result.outcome else { panic!() };
    let captured = captured.clone();
    runtime.finish(result.outcome).await;
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    let revisions = runtime.revisions(&captured.capture).await;
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].episode.first, recovered.first);
    assert_eq!(
        revisions[0].episode.recovery, recovered.recovery,
        "the earlier native recovery proof remains authoritative"
    );
    assert_eq!(revisions[0].episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert_eq!(revisions[0].episode.issue, Some(DiagnosticAnalysisIssue::NativeContextGone));
    runtime.reconcile().await;
    assert!(!runtime.enroll().await, "a terminal single revalidation cannot become repeated native polling");
    assert_eq!(runtime.native_calls(), [3; 5]);
}

#[tokio::test]
async fn uncertain_result_retires_the_full_intent_but_keeps_a_compact_replay_receipt() {
    let mut runtime = Runtime::new().await;
    runtime.fixture.backend.focus_request = PageWaitRequestState::Unverified;
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    let intent = runtime.fixture.journal.focused_intent(&job.key).await.unwrap().unwrap();
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: captured } = &result.outcome else { panic!() };
    let captured = captured.clone();
    runtime.finish(result.outcome).await;
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );

    let revisions = runtime.revisions(&captured.capture).await;
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].episode.lifecycle, DiagnosticLifecycle::Uncertain);
    assert_eq!(revisions[0].episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert_eq!(revisions[0].episode.issue, Some(DiagnosticAnalysisIssue::EvidenceUnavailable));
    assert!(revisions[0].episode.recovery.is_none(), "an unverified request proves no recovery");
    runtime.reconcile().await;
    assert!(runtime.fixture.journal.focused_intent(&job.key).await.unwrap().is_none());
    assert!(runtime.scheduler.focused_keys().is_empty());
    assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
    assert!(matches!(
        runtime.fixture.journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap(),
        Some(FocusedStage::Completed)
    ));
    let receipt = runtime.fixture.journal.focused_receipt(&intent.episode_stage_key().unwrap()).await.unwrap().unwrap();
    assert_eq!(receipt.key, job.key);
    assert_eq!(receipt.episode_id, intent.episode_id);
    assert_eq!(receipt.settlement, FocusedSettlement::Completed { result: captured });
    assert!(
        !runtime.enroll().await,
        "the consumed stage cannot silently repeat while native visibility remains uncertain"
    );
    assert_eq!(runtime.native_calls(), [2; 5]);
}

#[tokio::test]
async fn route_replacement_after_enrollment_withholds_before_factory_or_capture() {
    let mut runtime = Runtime::new().await;
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    runtime.fixture.backend.changed.store(true, Ordering::SeqCst);
    let result = runtime.execute(&job).await;
    assert_eq!(result.event.status, CollectionStatus::Incomplete(CollectionStop::SourceChanged));
    assert!(result.event.capture.is_none());
    assert!(matches!(result.outcome, Outcome::FocusedRetry { attempt: None, .. }));
    assert_eq!(runtime.native_calls(), [1; 5]);
    let intent = runtime.fixture.journal.focused_intent(&job.key).await.unwrap().unwrap();
    let input = runtime
        .fixture
        .outbox
        .diagnostic_input(&intent.focus.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    let episode =
        input.checkpoint.unwrap().episodes.into_iter().find(|episode| episode.id == intent.episode_id).unwrap();
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Deferred);
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::SourceChanged));
    runtime.finish(result.outcome).await;
}

#[tokio::test]
async fn quiet_failed_and_withheld_attempts_can_retire_after_analysis_becomes_terminal() {
    for failed_read in [true, false] {
        let mut runtime = Runtime::new().await;
        assert!(runtime.enroll().await);
        let job = runtime.take_focus().await;
        let intent = runtime.fixture.journal.focused_intent(&job.key).await.unwrap().unwrap();
        let original = runtime.fixture.outbox.diagnostic_evidence(&intent.focus.parent).await.unwrap();
        let before = runtime
            .fixture
            .outbox
            .diagnostic_input(&intent.focus.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
            .await
            .unwrap()
            .checkpoint
            .unwrap()
            .episodes
            .into_iter()
            .find(|episode| episode.id == intent.episode_id)
            .unwrap();
        let expected_work = if failed_read {
            runtime.fixture.backend.focus_failure = Some(CollectionStop::SourceNotReady);
            SourceWorkState::Quiescent
        } else {
            runtime.fixture.backend.changed.store(true, Ordering::SeqCst);
            SourceWorkState::NotStarted
        };
        let result = runtime.execute(&job).await;
        assert!(matches!(&result.outcome, Outcome::FocusedRetry { source_work, .. } if *source_work == expected_work));
        let calls = if failed_read { [2, 2, 2, 1, 2] } else { [1; 5] };
        assert_eq!(
            runtime.native_calls(),
            calls,
            "a failed read still closes its native owner; a withheld attempt never opens it"
        );
        runtime.finish(result.outcome).await;
        let state = runtime.scheduler.focused_state(&job.key).unwrap();
        assert_eq!(runtime.fixture.journal.scheduler().await.unwrap().unwrap().focused[&job.key].attempts, 1);
        assert_eq!(state.source_work, expected_work);
        assert!(runtime.fixture.journal.focused_intent(&job.key).await.unwrap().is_some());
        let current = runtime
            .fixture
            .outbox
            .diagnostic_input(&intent.focus.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
            .await
            .unwrap()
            .checkpoint
            .unwrap()
            .episodes
            .into_iter()
            .find(|episode| episode.id == intent.episode_id)
            .unwrap();
        assert!(current.revision.parse::<u64>().unwrap() > before.revision.parse::<u64>().unwrap());
        let mut expected = before;
        expected.revision = current.revision.clone();
        expected.analysis = DiagnosticAnalysisStatus::Deferred;
        expected.issue = Some(if failed_read {
            DiagnosticAnalysisIssue::SourceUnavailable { code: None, stopped: Some(CollectionStop::SourceNotReady) }
        } else {
            DiagnosticAnalysisIssue::SourceChanged
        });
        assert!(current.investigation_event.is_some(), "every settled attempt has immutable owner history");
        expected.investigation_event = current.investigation_event.clone();
        assert_eq!(
            current, expected,
            "settled retry changes analysis state without rewriting native lifecycle, scope or evidence"
        );
        assert_eq!(runtime.fixture.outbox.diagnostic_evidence(&intent.focus.parent).await.unwrap(), original);
        assert_eq!(runtime.native_calls(), calls, "persisted retry-state inspection performs no native work");

        set_analysis(
            &mut runtime.fixture.outbox,
            &intent.focus.parent,
            &intent.episode_id,
            DiagnosticAnalysisStatus::Unresolved,
            Some(DiagnosticAnalysisIssue::EvidenceUnavailable),
        )
        .await
        .unwrap();
        runtime.reconcile().await;
        assert!(runtime.fixture.journal.focused_intent(&job.key).await.unwrap().is_none());
        assert!(runtime.scheduler.focused_keys().is_empty());
        assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
        let receipt =
            runtime.fixture.journal.focused_receipt(&intent.episode_stage_key().unwrap()).await.unwrap().unwrap();
        assert_eq!(receipt.key, job.key);
        assert_eq!(receipt.settlement, FocusedSettlement::Cancelled { reason: DiagnosticAnalysisIssue::PolicyChanged });
        assert_eq!(runtime.native_calls(), calls, "terminal settlement cannot open another source");
    }
}

#[tokio::test]
async fn an_interrupted_admission_cannot_release_its_parent_when_analysis_later_ends() {
    let mut runtime = Runtime::new().await;
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    let intent = runtime.fixture.journal.focused_intent(&job.key).await.unwrap().unwrap();
    assert_eq!(runtime.native_calls(), [1; 5], "the test interrupts after durable admission and before the callback");
    let saved = runtime.fixture.journal.scheduler().await.unwrap().unwrap();
    let mut restored = Scheduler::new(&runtime.fixture.inventory, "one").unwrap();
    let now = std::time::Duration::ZERO;
    restored.restore(saved, now, chrono::Utc::now()).unwrap();
    let update = restored.resumed_admission_checkpoint(now, chrono::Utc::now()).unwrap().unwrap();
    restored.persisted(runtime.fixture.journal.update_scheduler(update).await.unwrap()).unwrap();
    runtime.scheduler = restored;
    runtime.started = Instant::now();
    assert_eq!(runtime.scheduler.focused_state(&job.key).unwrap().source_work, SourceWorkState::Unconfirmed);

    set_analysis(
        &mut runtime.fixture.outbox,
        &intent.focus.parent,
        &intent.episode_id,
        DiagnosticAnalysisStatus::Unresolved,
        Some(DiagnosticAnalysisIssue::EvidenceUnavailable),
    )
    .await
    .unwrap();
    runtime.reconcile().await;
    assert!(runtime.fixture.journal.focused_intent(&job.key).await.unwrap().is_some());
    assert_eq!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap(), vec![job.key.clone()]);
    assert_eq!(runtime.scheduler.focused_state(&job.key).unwrap().source_work, SourceWorkState::Unconfirmed);
    assert_eq!(runtime.native_calls(), [1; 5], "recovery must not infer native shutdown or run a replacement source");
}

#[tokio::test]
async fn unqualified_catalog_or_refused_cost_artifact_never_enrolls_or_opens_a_focus() {
    for unqualified in [true, false] {
        let mut runtime = Runtime::new().await;
        if unqualified {
            runtime.fixture.backend.recipe.cost = RecipeCostQualification::Unqualified;
        } else {
            runtime.fixture.backend.accept = false;
        }
        assert!(!runtime.enroll().await);
        assert!(runtime.fixture.journal.focused_intents().await.unwrap().is_empty());
        assert!(runtime.scheduler.focused_keys().is_empty());
        assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
        assert_eq!(runtime.native_calls(), [1; 5]);
        assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), usize::from(!unqualified));
        if unqualified {
            assert_eq!(runtime.fixture.backend.components.load(Ordering::SeqCst), 0);
        }
    }
}
