use super::*;
use crate::delivery::{
    contract::Contract,
    diagnostic_recovery::{load_diagnostic_checkpoint, load_diagnostic_evidence},
    file_store::FileRecoveryStore,
    recovery::{RecoveryStore, stage_and_commit},
};

async fn owner_events(runtime: &Runtime) -> Vec<(DiagnosticEvidenceRef, DiagnosticInvestigationEvent)> {
    let mut history = Vec::new();
    let entries = runtime.fixture.outbox.status(100, None).await.unwrap().entries;
    for entry in entries.into_iter().filter(|entry| entry.request.collection() == TelemetryCollection::Diagnostics) {
        let destination = runtime.fixture._directory.path().join(format!("history-{}", entry.capture.observation_id));
        let rows = replay(&runtime.fixture._directory.path().join("outbox"), &entry.capture, &destination).await;
        let bytes = std::fs::read(destination.join("manifest.json")).unwrap();
        let manifest: CollectionManifest = serde_json::from_slice(&bytes).unwrap();
        let source = manifest.description.as_ref().unwrap();
        for (ordinal, row) in rows.iter().enumerate() {
            if let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::DiagnosticRevision(revision),
                ..
            }) = &row.record
                && let Some(event) = &revision.investigation
            {
                let reference =
                    DiagnosticEvidenceRef::from_export(source, &digest(&bytes), row, ordinal as u32).unwrap();
                history.push((reference, event.as_ref().clone()));
            }
        }
    }
    history
}

#[tokio::test]
async fn authentication_and_description_failures_recover_full_owner_history_without_publishing_native_prefixes() {
    for authentication in [true, false] {
        let archive = tempfile::tempdir().unwrap();
        let contract = Contract::new(
            "pipeline".into(),
            "shard".into(),
            "file:warehouse".into(),
            digest(b"failed-attempt-history"),
        )
        .unwrap();
        let recovery = FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap();
        let owner = recovery.acquire().await.unwrap();
        let fixture = Harness::with_recovery_store(1, Some(owner.fence())).await;
        let mut runtime = Runtime::from_fixture(fixture).await;
        assert!(runtime.enroll().await);
        let job = runtime.take_focus().await;
        let intent = runtime.fixture.journal.focused_intent(&job.key).await.unwrap().unwrap();
        runtime.fixture.backend.focus_auth_failure = authentication;
        runtime.fixture.backend.focus_failure = (!authentication).then_some(CollectionStop::SourceNotReady);
        let result = runtime.execute(&job).await;
        let Outcome::FocusedRetry { attempt: Some(attempt), source_work: SourceWorkState::Quiescent, .. } =
            &result.outcome
        else {
            panic!("a failed attempt retains its allocation and explicit settlement")
        };
        let attempt = attempt.clone();
        assert_eq!(result.event.capture.as_ref(), Some(&attempt));
        let bytes = std::fs::read(
            runtime
                .fixture
                ._directory
                .path()
                .join("outbox/captures")
                .join(&attempt.observation_id)
                .join("manifest.json"),
        )
        .unwrap();
        let manifest: CollectionManifest = serde_json::from_slice(&bytes).unwrap();
        assert!(matches!(manifest.status, CollectionStatus::Incomplete(_)));
        assert!(manifest.description.is_none());
        assert_eq!(manifest.investigation_intent().unwrap(), Some(intent.clone()));
        let history = owner_events(&runtime).await;
        assert_eq!(history.len(), 2);
        let (reference, stopped) = history
            .iter()
            .find(|(_, event)| matches!(event.outcome, DiagnosticInvestigationOutcome::Stopped { .. }))
            .unwrap();
        assert_eq!(stopped.intent.key().unwrap(), intent.key().unwrap());
        assert_eq!(stopped.attempt.as_ref().unwrap().capture, attempt);
        assert!(stopped.attempt.as_ref().unwrap().manifest_sha256.is_some());
        let expected_code = authentication.then_some(culprit_core::ErrorCode::AuthFailed);
        assert!(matches!(&stopped.outcome, DiagnosticInvestigationOutcome::Stopped {
            issue: DiagnosticAnalysisIssue::SourceUnavailable { code, .. }
        } if *code == expected_code));
        assert_eq!(result.event.error_code, expected_code);
        assert!(history.iter().any(|(_, event)| event.outcome == DiagnosticInvestigationOutcome::Admitted));
        let event_key = stopped.key().unwrap();
        runtime.finish(result.outcome).await;
        let calls = runtime.native_calls();
        let mut entries = runtime.fixture.outbox.status(100, None).await.unwrap().entries;
        assert_eq!(
            entries.iter().find(|entry| entry.capture == attempt).unwrap().state,
            OutboxCaptureState::Incomplete
        );
        entries.sort_by_key(|entry| entry.capture.revision.as_ref().unwrap().sequence.parse::<u64>().unwrap());
        for entry in entries.into_iter().filter(|entry| entry.state == OutboxCaptureState::Ready) {
            let prepared = runtime
                .fixture
                .outbox
                .delivery()
                .prepare(&entry.capture.observation_id, contract.clone(), vec![])
                .await
                .unwrap();
            let head = recovery.checkpoint_head().await.unwrap();
            let committed = stage_and_commit(&recovery, owner.fence(), head.as_deref(), &prepared).await.unwrap();
            runtime.fixture.outbox.delivery().mark_archived(&committed).await.unwrap();
        }
        let heads = recovery.diagnostic_heads(None, 10).await.unwrap();
        assert_eq!(heads.heads.len(), 1);
        let cold_directory = tempfile::tempdir().unwrap();
        let mut cold =
            Outbox::create_remote(&cold_directory.path().join("outbox"), OutboxLimits::default(), recovery.id())
                .unwrap();
        cold.recover().await.unwrap();
        cold.restore_diagnostics(load_diagnostic_checkpoint(&recovery, &heads.heads[0]).await.unwrap()).await.unwrap();
        let restored = cold.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap();
        let episode = restored
            .checkpoints
            .iter()
            .flat_map(|checkpoint| &checkpoint.episodes)
            .find(|episode| episode.id == intent.episode_id)
            .unwrap();
        assert_eq!(episode.investigation_event.as_deref(), Some(event_key.as_str()));
        assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Deferred);
        assert!(episode.analysis.needs_work());
        let (_, recovered) = load_diagnostic_evidence(&recovery, reference).await.unwrap().into_parts();
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::DiagnosticRevision(revision),
            ..
        }) = recovered.record
        else {
            panic!()
        };
        assert_eq!(revision.investigation.as_deref(), Some(stopped));
        assert_eq!(
            runtime.native_calls(),
            calls,
            "archive reads and cold restoration need no source credentials or native calls"
        );
        assert!(
            recovery.capture_manifest(&attempt).await.is_err(),
            "the failed native prefix was never published as complete"
        );
    }
}

#[tokio::test]
async fn uncertain_native_shutdown_keeps_owner_history_and_the_active_scheduler_obligation() {
    let mut runtime = Runtime::new().await;
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    runtime.fixture.backend.focus_failure = Some(CollectionStop::CancellationUnconfirmed);
    let admitted = AdmittedBackend::new(&runtime.fixture.backend, Instant::now());
    let error = runtime
        .investigations
        .execute(
            &runtime.fixture.backend,
            &admitted,
            &runtime.fixture.journal,
            &mut CaptureStorage::local(&mut runtime.fixture.outbox),
            &job,
            &CollectionCancellation::default(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(admitted.source_work(), SourceWorkState::Unconfirmed);
    let receipt = admitted.attempt().unwrap().unwrap();
    let events = owner_events(&runtime).await;
    let (_, stopped) = events
        .iter()
        .find(|(_, event)| matches!(event.outcome, DiagnosticInvestigationOutcome::Stopped { .. }))
        .unwrap();
    assert_eq!(stopped.attempt.as_ref(), Some(&receipt));
    assert_eq!(stopped.source_work, SourceWorkState::Unconfirmed);
    assert_eq!(runtime.fixture.outbox.status(100, None).await.unwrap().unconfirmed_sources, 1);
    assert!(runtime.fixture.journal.focused_intent(&job.key).await.unwrap().is_some());
    assert_eq!(runtime.scheduler.focused_state(&job.key).unwrap().source_work, SourceWorkState::Active);
    assert_eq!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap(), vec![job.key]);
}
