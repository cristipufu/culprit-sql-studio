use super::*;
use crate::{
    delivery::{
        contract::Contract,
        diagnostic_recovery::{load_diagnostic_checkpoint, load_diagnostic_input},
        file_store::FileRecoveryStore,
        recovery::{RecoveryStore, stage_and_commit},
    },
    diagnostics::{DiagnosticPolicies, drain_pending},
    fleet::{
        admission::AdmittedBackend,
        investigations::tests::support::{Harness, capture_frame},
        scheduler::{Outcome, Work},
    },
};

async fn scheduler(harness: &Harness) -> Scheduler {
    let mut scheduler = Scheduler::new(&harness.inventory, "one").unwrap();
    let wall = chrono::Utc::now();
    let state = scheduler.initial_state(Duration::ZERO, wall).unwrap();
    scheduler.persisted(harness.journal.initialize_scheduler(state).await.unwrap()).unwrap();
    assert!(matches!(scheduler.take(Duration::ZERO).unwrap(), Some(Work::Scheduled(_))));
    let state = scheduler.admission_checkpoint(Duration::ZERO, wall).unwrap();
    scheduler.persisted(harness.journal.update_scheduler(state).await.unwrap()).unwrap();
    scheduler.finish(Duration::ZERO, Outcome::Complete).unwrap();
    let state = scheduler.completion_checkpoint(Duration::ZERO, wall).unwrap();
    scheduler.persisted(harness.journal.update_scheduler(state).await.unwrap()).unwrap();
    scheduler
}
fn investigations(harness: &mut Harness) -> Investigations {
    let mut investigations = Investigations::new(&harness.inventory, "one");
    investigations.bindings = std::mem::take(&mut harness.bindings);
    investigations
}
async fn enqueue(harness: &mut Harness, scheduler: &mut Scheduler) -> DiagnosticInvestigationIntent {
    let candidate = harness.prepare(&CollectionCancellation::default()).await.unwrap().pop().unwrap();
    let key = candidate.intent.key().unwrap();
    harness.outbox.pin_investigation_parent(&key, &candidate.intent.focus).await.unwrap();
    let FocusEnrollment::Enqueued(update) =
        scheduler.enqueue_focus(candidate.job, Duration::ZERO, chrono::Utc::now()).unwrap()
    else {
        panic!()
    };
    scheduler.persisted(harness.journal.enqueue_focused(candidate.intent.clone(), *update).await.unwrap()).unwrap();
    candidate.intent
}
async fn recover_symptom(harness: &mut Harness, manager: &mut Investigations) {
    let mut recovered = harness.backend.activity.clone();
    recovered.records.clear();
    let SourceObservation::Activity(frame) = &mut recovered.source.observation else { panic!() };
    frame.at = "2020-01-01T12:01:00Z".into();
    frame.observed_from = frame.at.clone();
    frame.observed_until = frame.at.clone();
    frame.native_rows = 0;
    frame.observations = 0;
    frame.coverage = StateSourceCoverage::collected(recovered.source.source.clone(), 0);
    capture_frame(&mut harness.outbox, recovered.clone()).await;
    let policies = DiagnosticPolicies::from_inventory(&harness.inventory, "one").unwrap();
    drain_pending(&mut harness.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    manager.bindings.observe("target", &recovered.source, &harness.backend.revision).unwrap();
}

#[tokio::test]
async fn startup_reconciles_orphan_owners_without_removing_or_reopening_canonical_pending_work() {
    let mut harness = Harness::new(1).await;
    let mut scheduler = scheduler(&harness).await;
    let intent = enqueue(&mut harness, &mut scheduler).await;
    let key = intent.key().unwrap();
    let orphan = digest(b"crash-before-canonical-enrollment");
    harness.outbox.pin_investigation_parent(&orphan, &intent.focus).await.unwrap();
    let mut manager = Investigations::new(&harness.inventory, "one");
    let source_calls = harness.backend.calls.opened.load(std::sync::atomic::Ordering::SeqCst);
    manager
        .reconcile(
            &harness.backend,
            &harness.journal,
            &mut CaptureStorage::local(&mut harness.outbox),
            &mut scheduler,
            tokio::time::Instant::now(),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(harness.outbox.investigation_parent_keys().await.unwrap().as_slice(), std::slice::from_ref(&key));
    assert_eq!(harness.journal.focused_intent(&key).await.unwrap(), Some(intent));
    assert_eq!(harness.backend.calls.opened.load(std::sync::atomic::Ordering::SeqCst), source_calls);
}

#[tokio::test]
async fn qualified_symptom_recovery_preserves_owed_work_and_does_not_claim_the_native_request_is_gone() {
    let mut harness = Harness::new(1).await;
    let mut scheduler = scheduler(&harness).await;
    let intent = enqueue(&mut harness, &mut scheduler).await;
    let mut manager = investigations(&mut harness);
    recover_symptom(&mut harness, &mut manager).await;
    manager
        .reconcile(
            &harness.backend,
            &harness.journal,
            &mut CaptureStorage::local(&mut harness.outbox),
            &mut scheduler,
            tokio::time::Instant::now(),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    let input = harness
        .outbox
        .diagnostic_input(&intent.focus.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    let episode =
        input.checkpoint.unwrap().episodes.into_iter().find(|episode| episode.id == intent.episode_id).unwrap();
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert!(episode.analysis.needs_work());
    assert!(episode.recovery.is_some());
    assert_ne!(episode.issue, Some(DiagnosticAnalysisIssue::NativeContextGone));
    assert_eq!(harness.journal.focused_intent(&intent.key().unwrap()).await.unwrap(), Some(intent));
}

#[tokio::test]
async fn a_stale_never_started_route_cancels_durably_without_changing_native_resource_history() {
    let mut harness = Harness::new(1).await;
    let mut scheduler = scheduler(&harness).await;
    let intent = enqueue(&mut harness, &mut scheduler).await;
    let mut manager = investigations(&mut harness);
    harness.backend.changed.store(true, std::sync::atomic::Ordering::SeqCst);
    let before = harness.journal.scheduler().await.unwrap().unwrap();
    let opened = harness.backend.calls.opened.load(std::sync::atomic::Ordering::SeqCst);
    manager
        .reconcile(
            &harness.backend,
            &harness.journal,
            &mut CaptureStorage::local(&mut harness.outbox),
            &mut scheduler,
            tokio::time::Instant::now(),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    assert!(harness.journal.focused_intents().await.unwrap().is_empty());
    assert!(harness.outbox.investigation_parent_keys().await.unwrap().is_empty());
    let receipt = harness.journal.focused_receipt(&intent.episode_stage_key().unwrap()).await.unwrap().unwrap();
    assert_eq!(
        receipt.settlement,
        crate::fleet::journal::FocusedSettlement::Cancelled { reason: DiagnosticAnalysisIssue::SourceChanged }
    );
    let after = harness.journal.scheduler().await.unwrap().unwrap();
    let resource = &harness.inventory.targets[0].admission_group;
    let before = serde_json::to_value(&before.resources[resource]).unwrap();
    let after = serde_json::to_value(&after.resources[resource]).unwrap();
    assert_eq!(after["attempts"], before["attempts"]);
    assert_eq!(after["pressure_failures"], before["pressure_failures"]);
    assert_eq!(after["max_reservation_ms"], before["max_reservation_ms"]);
    assert_eq!(harness.backend.calls.opened.load(std::sync::atomic::Ordering::SeqCst), opened);
    let current = harness
        .outbox
        .diagnostic_input(&intent.focus.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    let episode =
        current.checkpoint.unwrap().episodes.into_iter().find(|episode| episode.id == intent.episode_id).unwrap();
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Deferred);
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::SourceChanged));
}

#[tokio::test]
async fn declared_missing_metadata_cancels_only_the_queued_stage_and_keeps_periodic_work_runnable() {
    for code in [culprit_core::ErrorCode::NotFound, culprit_core::ErrorCode::Unsupported] {
        let mut harness = Harness::new(1).await;
        let mut scheduler = scheduler(&harness).await;
        let intent = enqueue(&mut harness, &mut scheduler).await;
        let mut manager = investigations(&mut harness);
        let jobs = harness.journal.scheduler().await.unwrap().unwrap().jobs.keys().cloned().collect::<BTreeSet<_>>();
        harness.backend.route_error = Some(culprit_core::Error::new(code, String::new()));
        let opened = harness.backend.calls.opened.load(std::sync::atomic::Ordering::SeqCst);
        manager
            .reconcile(
                &harness.backend,
                &harness.journal,
                &mut CaptureStorage::local(&mut harness.outbox),
                &mut scheduler,
                tokio::time::Instant::now(),
                &CollectionCancellation::default(),
            )
            .await
            .unwrap();
        assert!(harness.journal.focused_intent(&intent.key().unwrap()).await.unwrap().is_none());
        assert!(harness.outbox.investigation_parent_keys().await.unwrap().is_empty());
        let receipt = harness.journal.focused_receipt(&intent.episode_stage_key().unwrap()).await.unwrap().unwrap();
        assert_eq!(
            receipt.settlement,
            crate::fleet::journal::FocusedSettlement::Cancelled {
                reason: DiagnosticAnalysisIssue::SourceUnavailable { code: None, stopped: None },
            }
        );
        assert_eq!(
            harness.journal.scheduler().await.unwrap().unwrap().jobs.keys().cloned().collect::<BTreeSet<_>>(),
            jobs
        );
        assert!(matches!(
            scheduler.take_allowed(Duration::from_secs(86_400), |_| false).unwrap(),
            Some(Work::Scheduled(_))
        ));
        assert_eq!(harness.backend.calls.opened.load(std::sync::atomic::Ordering::SeqCst), opened);
        assert_eq!(harness.backend.calls.captured.load(std::sync::atomic::Ordering::SeqCst), 0);
        let input = harness
            .outbox
            .diagnostic_input(&intent.focus.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
            .await
            .unwrap();
        let episode =
            input.checkpoint.unwrap().episodes.into_iter().find(|episode| episode.id == intent.episode_id).unwrap();
        assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
        assert_eq!(episode.first, intent.focus.parent);
        assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Deferred);
    }
}

#[tokio::test]
async fn typed_integrity_and_storage_metadata_errors_preserve_the_queued_stage_and_fail_closed() {
    for problem in [DeliveryProblem::Integrity, DeliveryProblem::Storage] {
        let mut harness = Harness::new(1).await;
        let mut scheduler = scheduler(&harness).await;
        let intent = enqueue(&mut harness, &mut scheduler).await;
        let mut manager = investigations(&mut harness);
        harness.backend.route_error = Some(problem.error());
        let error = manager
            .reconcile(
                &harness.backend,
                &harness.journal,
                &mut CaptureStorage::local(&mut harness.outbox),
                &mut scheduler,
                tokio::time::Instant::now(),
                &CollectionCancellation::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(problem)));
        assert_eq!(harness.journal.focused_intent(&intent.key().unwrap()).await.unwrap(), Some(intent.clone()));
        assert_eq!(harness.outbox.investigation_parent_keys().await.unwrap(), [intent.key().unwrap()]);
        assert_eq!(harness.backend.calls.captured.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn metadata_removed_after_admission_withholds_execution_without_opening_a_native_source() {
    let mut harness = Harness::new(1).await;
    let mut scheduler = scheduler(&harness).await;
    let intent = enqueue(&mut harness, &mut scheduler).await;
    let manager = investigations(&mut harness);
    let Work::Focused(job) = scheduler.take(Duration::from_secs(60)).unwrap().unwrap() else { panic!() };
    let update = scheduler.admission_checkpoint(Duration::from_secs(60), chrono::Utc::now()).unwrap();
    scheduler.persisted(harness.journal.update_scheduler(update).await.unwrap()).unwrap();
    harness.backend.route_error = Some(culprit_core::Error::new(culprit_core::ErrorCode::NotFound, String::new()));
    let admitted = AdmittedBackend::new(&harness.backend, tokio::time::Instant::now());
    let result = manager
        .execute(
            &harness.backend,
            &admitted,
            &harness.journal,
            &mut CaptureStorage::local(&mut harness.outbox),
            &job,
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    assert!(matches!(result.outcome, Outcome::FocusedRetry { source_work: SourceWorkState::NotStarted, .. }));
    assert_eq!(harness.backend.calls.opened.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(harness.backend.calls.captured.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(harness.journal.focused_intent(&job.key).await.unwrap(), Some(intent));
}

#[tokio::test]
async fn an_interrupted_unconfirmed_admission_cannot_be_cancelled_by_a_later_route_change() {
    let mut harness = Harness::new(1).await;
    let mut scheduler = scheduler(&harness).await;
    let intent = enqueue(&mut harness, &mut scheduler).await;
    assert!(matches!(scheduler.take(Duration::from_secs(60)).unwrap(), Some(Work::Focused(_))));
    let update = scheduler.admission_checkpoint(Duration::from_secs(60), chrono::Utc::now()).unwrap();
    scheduler.persisted(harness.journal.update_scheduler(update).await.unwrap()).unwrap();
    let mut resumed = Scheduler::new(&harness.inventory, "one").unwrap();
    resumed.restore(harness.journal.scheduler().await.unwrap().unwrap(), Duration::ZERO, chrono::Utc::now()).unwrap();
    let rebase = resumed.resumed_admission_checkpoint(Duration::ZERO, chrono::Utc::now()).unwrap().unwrap();
    resumed.persisted(harness.journal.update_scheduler(rebase).await.unwrap()).unwrap();
    let mut manager = investigations(&mut harness);
    harness.backend.changed.store(true, std::sync::atomic::Ordering::SeqCst);
    manager
        .reconcile(
            &harness.backend,
            &harness.journal,
            &mut CaptureStorage::local(&mut harness.outbox),
            &mut resumed,
            tokio::time::Instant::now(),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    let state = resumed.focused_state(&intent.key().unwrap()).unwrap();
    assert_eq!(state.source_work, SourceWorkState::Unconfirmed);
    assert_eq!(harness.journal.focused_intent(&intent.key().unwrap()).await.unwrap(), Some(intent.clone()));
    assert_eq!(harness.outbox.investigation_parent_keys().await.unwrap(), [intent.key().unwrap()]);
}

#[tokio::test]
async fn an_interrupted_receipt_inventory_walk_never_turns_an_unvisited_head_into_absence() {
    let mut harness = Harness::new(1).await;
    let mut scheduler = scheduler(&harness).await;
    let intent = enqueue(&mut harness, &mut scheduler).await;
    let mut manager = investigations(&mut harness);
    harness.backend.changed.store(true, std::sync::atomic::Ordering::SeqCst);
    manager
        .reconcile(
            &harness.backend,
            &harness.journal,
            &mut CaptureStorage::local(&mut harness.outbox),
            &mut scheduler,
            tokio::time::Instant::now(),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(harness.journal.focused_receipt_episode_ids().await.unwrap(), [intent.episode_id.clone()].into());
    recover_symptom(&mut harness, &mut manager).await;
    set_analysis(
        &mut harness.outbox,
        &intent.focus.parent,
        &intent.episode_id,
        DiagnosticAnalysisStatus::Unresolved,
        Some(DiagnosticAnalysisIssue::Unsupported),
    )
    .await
    .unwrap();
    assert!(
        harness.outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 1).await.unwrap().checkpoints[0]
            .episodes
            .is_empty()
    );
    let stopped = CollectionCancellation::default();
    stopped.cancel();
    manager.prune_receipts(&harness.journal, &mut harness.outbox, Duration::from_secs(3_600), &stopped).await.unwrap();
    assert_eq!(
        harness.journal.focused_receipt_episode_ids().await.unwrap(),
        [intent.episode_id].into(),
        "an incomplete traversal retains every deduplication receipt"
    );
    manager
        .prune_receipts(
            &harness.journal,
            &mut harness.outbox,
            Duration::from_secs(3_600),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    assert!(harness.journal.focused_receipt_episode_ids().await.unwrap().is_empty());
}

#[test]
fn numerically_equal_cost_accounting_cannot_replace_the_original_immutable_intent_lineage() {
    let mut intent = crate::analytical::test_support::investigation_fixture().intent;
    intent.qualification.read_metrics.templates[0].client_elapsed_ms = 0.0;
    let mut changed = intent.clone();
    changed.qualification.read_metrics.templates[0].client_elapsed_ms = -0.0;
    assert_eq!(intent, changed, "numeric equality is weaker than canonical artifact identity");
    assert_ne!(intent.key().unwrap(), changed.key().unwrap());
    let result = ActivityWaitFocusResult {
        focus: intent.focus.clone(),
        lineage: Some(DiagnosticInvestigationLineage::from_intent(&intent).unwrap()),
        observed_from: "2020-01-01T12:00:01Z".into(),
        observed_until: "2020-01-01T12:00:02Z".into(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::Missing,
        wait: PageWaitRelation::Unqualified,
    };
    let mut source = CollectionDescription {
        target: intent.focus.target.clone(),
        source: "focused-page-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedPageWait {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitTargets,
                observed_from: result.observed_from.clone(),
                observed_until: result.observed_until.clone(),
                records: 0,
                sources: vec![StateSourceCoverage::not_applicable("page wait targets".into())],
            },
            result: Box::new(result),
        },
    };
    assert!(result_matches_intent(&source, &intent).unwrap());
    let SourceObservation::FocusedPageWait { result, .. } = &mut source.observation else { panic!() };
    result.lineage = Some(DiagnosticInvestigationLineage::from_intent(&changed).unwrap());
    assert!(result.is_valid(), "the other lineage is independently valid");
    assert!(!result_matches_intent(&source, &intent).unwrap());
}

#[tokio::test]
async fn remote_retirement_waits_for_exact_result_and_analysis_archive_then_accepts_evidence_only_hydration() {
    let archive = tempfile::tempdir().unwrap();
    let contract =
        Contract::new("pipeline".into(), "shard".into(), "file:warehouse".into(), digest(b"queue-recovery-schema"))
            .unwrap();
    let recovery = FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap();
    let owner = recovery.acquire().await.unwrap();
    let mut harness = Harness::with_recovery_store(1, Some(owner.fence())).await;
    let mut scheduler = scheduler(&harness).await;
    let intent = enqueue(&mut harness, &mut scheduler).await;
    let mut manager = investigations(&mut harness);
    let Work::Focused(job) = scheduler.take(Duration::from_secs(60)).unwrap().unwrap() else { panic!() };
    let update = scheduler.admission_checkpoint(Duration::from_secs(60), chrono::Utc::now()).unwrap();
    scheduler.persisted(harness.journal.update_scheduler(update).await.unwrap()).unwrap();
    let admitted = AdmittedBackend::new(&harness.backend, tokio::time::Instant::now());
    let result = manager
        .execute(
            &harness.backend,
            &admitted,
            &harness.journal,
            &mut CaptureStorage { outbox: &mut harness.outbox, recovery: Some(&recovery) },
            &job,
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!("sealed focused result") };
    let capture = capture.clone();
    scheduler.finish(Duration::from_secs(60), result.outcome).unwrap();
    let update = scheduler.completion_checkpoint(Duration::from_secs(60), chrono::Utc::now()).unwrap();
    scheduler.persisted(harness.journal.update_scheduler(update).await.unwrap()).unwrap();
    let policies = DiagnosticPolicies::from_inventory(&harness.inventory, "one").unwrap();
    drain_pending(&mut harness.outbox, &policies, &CollectionCancellation::default(), Some(&recovery)).await.unwrap();
    assert_eq!(
        harness.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Consumed
    );
    let started = tokio::time::Instant::now();
    manager
        .reconcile(
            &harness.backend,
            &harness.journal,
            &mut CaptureStorage { outbox: &mut harness.outbox, recovery: Some(&recovery) },
            &mut scheduler,
            started,
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    assert!(
        harness.journal.focused_intent(&job.key).await.unwrap().is_some(),
        "local analysis is not an archive acknowledgement"
    );
    assert_eq!(harness.outbox.investigation_parent_keys().await.unwrap().as_slice(), std::slice::from_ref(&job.key));

    let mut entries = harness.outbox.status(100, None).await.unwrap().entries;
    entries.sort_by_key(|entry| entry.capture.revision.as_ref().unwrap().sequence.parse::<u64>().unwrap());
    let mut analysis = None;
    for entry in entries {
        if matches!(&entry.request.selection, CollectionSelection::Diagnostics { parent, .. } if parent == &capture) {
            assert!(analysis.replace(entry).is_none());
            continue;
        }
        let prepared =
            harness.outbox.delivery().prepare(&entry.capture.observation_id, contract.clone(), vec![]).await.unwrap();
        let head = recovery.checkpoint_head().await.unwrap();
        let committed = stage_and_commit(&recovery, owner.fence(), head.as_deref(), &prepared).await.unwrap();
        harness.outbox.delivery().mark_archived(&committed).await.unwrap();
    }
    assert!(recovery.diagnostic_input_pending(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap());
    assert_eq!(
        harness.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Consumed,
        "durable consumption metadata can outlive an archived payload",
    );
    assert_eq!(
        harness
            .outbox
            .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
            .await
            .unwrap_err()
            .detail,
        Some(culprit_core::ErrorDetail::Outbox(OutboxProblem::MissingCapture)),
        "archive acknowledgement already evicted the native payload before the derived archive acknowledgement",
    );
    manager
        .reconcile(
            &harness.backend,
            &harness.journal,
            &mut CaptureStorage { outbox: &mut harness.outbox, recovery: Some(&recovery) },
            &mut scheduler,
            started,
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    assert!(
        harness.journal.focused_intent(&job.key).await.unwrap().is_some(),
        "archiving a native result without its analysis still owes work"
    );
    assert_eq!(harness.outbox.investigation_parent_keys().await.unwrap().as_slice(), std::slice::from_ref(&job.key));

    let analysis = analysis.unwrap();
    let prepared =
        harness.outbox.delivery().prepare(&analysis.capture.observation_id, contract.clone(), vec![]).await.unwrap();
    let head = recovery.checkpoint_head().await.unwrap();
    let committed = stage_and_commit(&recovery, owner.fence(), head.as_deref(), &prepared).await.unwrap();
    harness.outbox.delivery().mark_archived(&committed).await.unwrap();
    assert!(!recovery.diagnostic_input_pending(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap());
    manager
        .reconcile(
            &harness.backend,
            &harness.journal,
            &mut CaptureStorage { outbox: &mut harness.outbox, recovery: Some(&recovery) },
            &mut scheduler,
            started,
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    assert!(harness.journal.focused_intent(&job.key).await.unwrap().is_none());
    assert!(harness.outbox.investigation_parent_keys().await.unwrap().is_empty());

    let heads = recovery.diagnostic_heads(None, 10).await.unwrap();
    assert_eq!(heads.heads.len(), 1);
    let cold_root = tempfile::tempdir().unwrap();
    let mut cold =
        crate::outbox::Outbox::create_remote(&cold_root.path().join("outbox"), OutboxLimits::default(), recovery.id())
            .unwrap();
    cold.recover().await.unwrap();
    cold.restore_diagnostics(load_diagnostic_checkpoint(&recovery, &heads.heads[0]).await.unwrap()).await.unwrap();
    let native = recovery.capture_manifest(&capture.capture).await.unwrap();
    cold.cache_diagnostic_evidence("saved-source", load_diagnostic_input(&recovery, &native.range_id).await.unwrap())
        .await
        .unwrap();
    assert_eq!(
        cold.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::EvidenceOnly
    );
    assert!(
        manager
            .result_consumed(
                &mut CaptureStorage { outbox: &mut cold, recovery: Some(&recovery) },
                &intent,
                &capture,
                &CollectionCancellation::default()
            )
            .await
            .unwrap()
    );
    assert_eq!(
        cold.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::EvidenceOnly,
        "remote authentication does not rewrite local queue knowledge"
    );
    assert!(cold.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
}
