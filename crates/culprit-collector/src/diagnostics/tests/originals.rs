use super::*;
use crate::analytical::test_support::{DiagnosticOriginalCase as Case, diagnostic_original_evidence};

fn observe(
    source: &CollectionDescription,
    rows: &[TelemetryRecord],
    previous: Option<&DiagnosticCheckpoint>,
    id: &str,
) -> DiagnosticObservationUpdate {
    let capture = CaptureIdentity {
        observation_id: id.into(),
        revision: Some(CaptureRevision {
            outbox_id: "original-source-observer".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let manifest = digest(id.as_bytes());
    let exports =
        rows.iter().map(|record| ExportedRecord::new(source, &capture, record.clone()).unwrap()).collect::<Vec<_>>();
    let mut positions = std::collections::BTreeMap::new();
    let references = exports
        .iter()
        .map(|row| {
            let position = positions.entry(row.batch_id.as_str()).or_insert(0u32);
            let reference = DiagnosticEvidenceRef::from_export(source, &manifest, row, *position).unwrap();
            *position += 1;
            reference
        })
        .collect::<Vec<_>>();
    let original = exports
        .iter()
        .zip(&references)
        .map(|(row, reference)| DiagnosticOriginalRow { reference, record: &row.record })
        .collect::<Vec<_>>();
    let parent = DiagnosticCaptureRef {
        capture,
        manifest_sha256: manifest,
        source_scope_id: source.captured_range().unwrap().scope_id,
    };
    observe_diagnostic_original(source, &parent, &original, previous, "2020-01-01T12:01:00Z").unwrap()
}
fn source_at(source: &mut CollectionDescription, second: u32) {
    let from = format!("2020-01-01T12:00:{second:02}Z");
    let until = format!("2020-01-01T12:00:{:02}Z", second + 1);
    match &mut source.observation {
        SourceObservation::Snapshot(snapshot) => {
            snapshot.observed_from = from;
            snapshot.observed_until = until;
        }
        SourceObservation::QueryHistory(history) => {
            let owner = history.acquisition_owner.as_mut().unwrap();
            owner.observed_from = from;
            owner.observed_until = until;
        }
        _ => unreachable!(),
    }
}

#[test]
fn qualified_positive_source_conditions_keep_exact_originals_without_creating_a_focus_or_current_owner() {
    for (case, count) in [
        (Case::Memory, 2),
        (Case::MysqlError, 4),
        (Case::PostgresFailure, 2),
        (Case::Cleanup, 1),
        (Case::Temporary, 2),
        (Case::HistoryException, 1),
        (Case::HistoryAborted, 1),
    ] {
        let (source, rows) = diagnostic_original_evidence(case);
        let original = rows.clone();
        let update = observe(&source, &rows, None, "positive-original");
        assert_eq!(update.revisions.len(), count, "{case:?}");
        assert!(update.focuses.is_empty());
        for revision in &update.revisions {
            let episode = &revision.episode;
            assert!(episode.signal.activity_key().is_none());
            assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Unresolved);
            assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::Unsupported));
            assert!(matches!(episode.report, Some(DiagnosticReport::OriginalObservation { .. })));
            if case != Case::Cleanup {
                assert_eq!(episode.lifecycle, DiagnosticLifecycle::Uncertain);
                assert!(episode.recovery.is_none());
            }
            assert!(episode.is_valid());
        }
        assert_eq!(rows, original);
    }
}

#[test]
fn genuine_zero_unknown_usage_native_regular_or_unknown_outcome_and_explicit_negative_session_peers_are_not_incidents()
{
    for case in [
        Case::MemoryEmpty,
        Case::MysqlWarning,
        Case::CleanupEmpty,
        Case::CleanupDenied,
        Case::TemporaryZero,
        Case::TemporaryUnknown,
        Case::TemporaryNegativePeer,
        Case::HistoryRegular,
        Case::HistoryUnknown,
        Case::HistoryZero,
    ] {
        let (source, rows) = diagnostic_original_evidence(case);
        let update = observe(&source, &rows, None, "negative-original");
        assert!(update.revisions.is_empty(), "{case:?}");
        assert!(update.checkpoint.episodes.is_empty(), "{case:?}");
        assert!(update.focuses.is_empty());
    }
}

#[test]
fn reobserved_native_failure_slots_and_replaced_values_revise_existing_groups_without_inventing_new_occurrences() {
    let (mut source, rows) = diagnostic_original_evidence(Case::Memory);
    let first = observe(&source, &rows, None, "original-buffer");
    source_at(&mut source, 2);
    let repeated = observe(&source, &rows, Some(&first.checkpoint), "repeated-buffer");
    assert_eq!(repeated.checkpoint.episodes.len(), 2);
    for episode in &repeated.checkpoint.episodes {
        let before = first.checkpoint.episodes.iter().find(|old| old.id == episode.id).unwrap();
        assert_eq!(episode.first, before.first);
        assert_eq!(episode.revision, "2");
        let record = rows.iter().find(|record| record.native_identity() == episode.latest.native_key).unwrap();
        let first = ExportedRecord::new(&source, &before.latest.capture, record.clone()).unwrap();
        let latest = ExportedRecord::new(&source, &episode.latest.capture, record.clone()).unwrap();
        assert_eq!(first.record, latest.record, "the exact native body and slot are unchanged");
        assert_eq!(before.latest.payload_sha256, first.payload_sha256().unwrap());
        assert_eq!(episode.latest.payload_sha256, latest.payload_sha256().unwrap());
        assert_ne!(
            episode.latest.payload_sha256, before.latest.payload_sha256,
            "receipts bind distinct capture/batch identities"
        );
        assert_ne!(episode.latest.record_id, before.latest.record_id);
        assert_ne!(episode.latest.capture, before.latest.capture);
    }
    let (_, changed) = diagnostic_original_evidence(Case::MemoryChanged);
    source_at(&mut source, 4);
    let replacement = observe(&source, &changed, Some(&repeated.checkpoint), "changed-slot-values");
    assert_eq!(replacement.checkpoint.episodes.len(), 2);
    let mut changed_bodies = 0;
    for episode in &replacement.checkpoint.episodes {
        let original = rows.iter().find(|record| record.native_identity() == episode.first.native_key).unwrap();
        let later = changed.iter().find(|record| record.native_identity() == episode.latest.native_key).unwrap();
        changed_bodies += usize::from(original != later);
        assert_eq!(
            episode.first.payload_sha256,
            ExportedRecord::new(&source, &episode.first.capture, original.clone()).unwrap().payload_sha256().unwrap()
        );
        assert_eq!(
            episode.latest.payload_sha256,
            ExportedRecord::new(&source, &episode.latest.capture, later.clone()).unwrap().payload_sha256().unwrap()
        );
    }
    assert_eq!(changed_bodies, 1, "one changed native slot body, independent of capture-bound receipt changes");
    assert!(
        replacement
            .checkpoint
            .episodes
            .iter()
            .all(|episode| episode.lifecycle == DiagnosticLifecycle::Uncertain && episode.recovery.is_none())
    );
    let (mut empty, rows) = diagnostic_original_evidence(Case::MemoryEmpty);
    source_at(&mut empty, 6);
    let absent = observe(&empty, &rows, Some(&replacement.checkpoint), "buffer-retention-empty");
    assert_eq!(absent.checkpoint.episodes.len(), 2);
    assert!(absent.checkpoint.episodes.iter().all(|episode| episode.recovery.is_none()));
}

#[test]
fn same_query_store_native_interval_plan_outcome_reconciles_all_original_fragments_without_summing_or_counting_executions()
 {
    let (mut source, rows) = diagnostic_original_evidence(Case::HistoryException);
    let first = observe(&source, &rows, None, "history-first");
    let first_episode = &first.checkpoint.episodes[0];
    let (_, mut corrected) = diagnostic_original_evidence(Case::HistoryMultipleFragments);
    source_at(&mut source, 2);
    let second = observe(&source, &corrected, Some(&first.checkpoint), "history-active-flush");
    let episode = &second.checkpoint.episodes[0];
    assert_eq!(second.checkpoint.episodes.len(), 1);
    assert_eq!(episode.id, first_episode.id);
    assert_eq!(episode.first, first_episode.first);
    let Some(DiagnosticReport::OriginalObservation { peers, .. }) = &episode.report else { panic!() };
    assert_eq!(peers.len(), 1, "both native fragments are retained rather than selecting or summing one");
    assert_eq!(second.revisions[0].inputs.len(), 2);
    let TelemetryRecord::QueryRuntime(row) = &mut corrected[0] else { unreachable!() };
    row.key.native.id = "9998".into();
    row.executions = "2".into();
    source_at(&mut source, 4);
    let third = observe(&source, &corrected, Some(&second.checkpoint), "history-correction");
    assert_eq!(third.checkpoint.episodes.len(), 1);
    assert_eq!(third.checkpoint.episodes[0].id, first_episode.id);
    assert_eq!(third.checkpoint.episodes[0].lifecycle, DiagnosticLifecycle::Uncertain);
    assert!(third.checkpoint.episodes[0].recovery.is_none());
    source_at(&mut source, 6);
    let empty = observe(&source, &[], Some(&third.checkpoint), "history-retention-empty");
    assert_eq!(empty.checkpoint.episodes.len(), 1);
    assert!(empty.checkpoint.episodes[0].recovery.is_none());
}

#[test]
fn only_an_explicit_complete_same_native_cleanup_boundary_negative_resolves_and_keeps_the_original_revision() {
    let (source, rows) = diagnostic_original_evidence(Case::Cleanup);
    let first = observe(&source, &rows, None, "protected-boundary");
    assert_eq!(first.checkpoint.episodes.len(), 1);
    for case in [Case::CleanupDenied, Case::CleanupPartial] {
        let (mut next, rows) = diagnostic_original_evidence(case);
        source_at(&mut next, 2);
        let uncertain = observe(&next, &rows, Some(&first.checkpoint), "incomplete-boundary");
        assert_eq!(uncertain.checkpoint.episodes.len(), 1);
        assert!(uncertain.checkpoint.episodes[0].recovery.is_none());
    }
    let (mut next, rows) = diagnostic_original_evidence(Case::CleanupEmpty);
    source_at(&mut next, 4);
    let recovered = observe(&next, &rows, Some(&first.checkpoint), "confirmed-no-boundary");
    assert!(recovered.checkpoint.episodes.is_empty());
    let [revision] = recovered.revisions.as_slice() else { panic!() };
    assert_eq!(revision.episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(revision.episode.first, first.checkpoint.episodes[0].first);
    assert_eq!(revision.episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert!(revision.episode.recovery.as_ref().unwrap().matches_source(
        &revision.episode,
        &next,
        &recovered.checkpoint.input
    ));
}

#[test]
fn missing_native_member_historical_namespace_and_changed_process_never_borrow_current_identity_or_resolve_old_evidence()
 {
    let (mut unqualified, _) = diagnostic_original_evidence(Case::Memory);
    unqualified.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    assert!(diagnostic_original_source_kind(&unqualified).is_none());
    let (history, _) = diagnostic_original_evidence(Case::HistoryMissingNamespace);
    assert!(diagnostic_original_source_kind(&history).is_none());
    let (source, rows) = diagnostic_original_evidence(Case::Cleanup);
    let first = observe(&source, &rows, None, "before-native-reset");
    let (mut reset, rows) = diagnostic_original_evidence(Case::CleanupEmpty);
    source_at(&mut reset, 2);
    let SourcePartition::InstanceLocal { epoch, .. } = &mut reset.partition else { unreachable!() };
    *epoch = Some(digest(b"different native process"));
    let after = observe(&reset, &rows, Some(&first.checkpoint), "after-native-reset");
    assert_eq!(after.checkpoint.episodes.len(), 1);
    assert!(after.checkpoint.episodes[0].recovery.is_none());
}

#[tokio::test]
async fn actual_original_queue_commits_classified_facts_once_and_preserves_resolved_history_after_restart_without_source_work()
 {
    let mut harness = Harness::new().await;
    let (source, records) = diagnostic_original_evidence(Case::Cleanup);
    let identity = harness.capture(Frame { source, records }).await;
    assert_eq!(harness.work(1).await, 1);
    let first = harness.checkpoint().await.episodes.remove(0);
    assert!(matches!(first.signal, DiagnosticSignal::OriginalObservation { .. }));
    let (mut source, records) = diagnostic_original_evidence(Case::CleanupEmpty);
    source_at(&mut source, 2);
    let later = harness.capture(Frame { source, records }).await;
    assert_eq!(harness.work(1).await, 1);
    assert!(harness.checkpoint().await.episodes.is_empty());
    let revisions = harness.revisions(&later).await;
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(revisions[0].episode.first, first.first);
    assert_eq!(harness.work(1).await, 0);
    let original_evidence = harness.outbox.diagnostic_evidence(&first.first).await.unwrap();
    drop(harness.outbox);
    let mut reopened = Outbox::open(&harness.path).unwrap();
    assert_eq!(reopened.recover().await.unwrap().source_debts, 0);
    assert_eq!(reopened.diagnostic_evidence(&first.first).await.unwrap(), original_evidence);
    assert_eq!(
        analyze_pending(&mut reopened, &harness.policies, &CollectionCancellation::default(), 1, None).await.unwrap(),
        0
    );
    let checkpoints = reopened.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap();
    assert!(checkpoints.checkpoints[0].episodes.is_empty());
    let original = reopened.diagnostic_input(&identity.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    assert_eq!(original.parent.capture, identity);
    assert_eq!(harness.calls.counts(), [2, 2, 2, 2]);
}

#[tokio::test]
async fn actual_queue_reconciles_repeated_memory_and_history_sources_and_healthy_negatives_with_no_native_reopen() {
    for (case, expected) in [(Case::Memory, 2), (Case::HistoryException, 1)] {
        let mut harness = Harness::new().await;
        let (source, records) = diagnostic_original_evidence(case);
        let first = harness.capture(Frame { source: source.clone(), records: records.clone() }).await;
        assert_eq!(harness.work(1).await, 1);
        let original = harness.checkpoint().await.episodes;
        let mut next = source;
        source_at(&mut next, 2);
        let repeated = harness.capture(Frame { source: next, records }).await;
        assert_eq!(harness.work(1).await, 1);
        let checkpoint = harness.checkpoint().await;
        assert_eq!(checkpoint.episodes.len(), expected);
        for episode in checkpoint.episodes {
            assert!(original.iter().any(|before| before.id == episode.id && before.first == episode.first));
            assert_eq!(episode.revision, "2");
        }
        assert_eq!(harness.revisions(&first).await.len(), expected);
        assert_eq!(harness.revisions(&repeated).await.len(), expected);
        assert_eq!(harness.work(1).await, 0);
        assert_eq!(harness.calls.counts(), [2, 2, 2, 2]);
    }
    for case in [Case::TemporaryZero, Case::TemporaryNegativePeer, Case::HistoryRegular] {
        let mut harness = Harness::new().await;
        let (source, records) = diagnostic_original_evidence(case);
        let identity = harness.capture(Frame { source, records }).await;
        assert_eq!(harness.work(1).await, 1);
        assert!(harness.checkpoint().await.episodes.is_empty());
        assert!(harness.revisions(&identity).await.is_empty());
        assert_eq!(harness.calls.counts(), [1, 1, 1, 1]);
    }
}

#[test]
fn a_source_group_cannot_omit_an_original_fragment_or_borrow_changed_native_values_under_a_receipt() {
    let (source, rows) = diagnostic_original_evidence(Case::HistoryMultipleFragments);
    let update = observe(&source, &rows, None, "complete-original-fragments");
    let episode = &update.checkpoint.episodes[0];
    let exports = rows
        .iter()
        .map(|row| ExportedRecord::new(&source, &episode.first.capture, row.clone()).unwrap())
        .collect::<Vec<_>>();
    let originals = exports
        .iter()
        .zip(&update.revisions[0].inputs)
        .map(|(row, reference)| DiagnosticOriginalRow { reference, record: &row.record })
        .collect::<Vec<_>>();
    let evidence = DiagnosticOriginalEvidence::new(&source, &episode.first.capture_ref(), &originals).unwrap();
    assert!(evidence.matches_episode(episode));
    let mut omitted = episode.clone();
    let Some(DiagnosticReport::OriginalObservation { peers, .. }) = &mut omitted.report else { unreachable!() };
    peers.clear();
    assert!(omitted.is_valid(), "individual references remain structurally valid");
    assert!(!evidence.matches_episode(&omitted), "the complete same-input population owns group admission");
    let mut changed = exports;
    let TelemetryRecord::QueryRuntime(row) = &mut changed[0].record else { unreachable!() };
    row.executions = "8".into();
    let originals = changed
        .iter()
        .zip(&update.revisions[0].inputs)
        .map(|(row, reference)| DiagnosticOriginalRow { reference, record: &row.record })
        .collect::<Vec<_>>();
    assert!(DiagnosticOriginalEvidence::new(&source, &episode.first.capture_ref(), &originals).is_err());
}

#[tokio::test]
async fn original_source_reports_and_the_complete_cleanup_negative_use_the_existing_archive_authentication_and_recovery_owner()
 {
    use crate::delivery::{contract::Contract, file_store::FileRecoveryStore, recovery::RecoveryStore};
    for case in [Case::Memory, Case::Cleanup] {
        let archive = tempfile::tempdir().unwrap();
        let contract = Contract::new(
            "pipeline".into(),
            "shard".into(),
            "file:source-originals".into(),
            digest(b"original-source-schema"),
        )
        .unwrap();
        let store = FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap();
        let owner = store.acquire().await.unwrap();
        let mut harness = Harness::with_recovery_store(Some(store.id())).await;
        harness.outbox.begin_remote_epoch(owner.fence(), 0).await.unwrap();
        let (source, records) = diagnostic_original_evidence(case);
        let native = harness.capture(Frame { source, records }).await;
        assert_eq!(harness.work(1).await, 1);
        super::recovery::archive_capture(&harness.outbox, &store, owner.fence(), &contract, &native).await;
        let derived = harness
            .outbox
            .status(10, None)
            .await
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| matches!(entry.request.selection, CollectionSelection::Diagnostics { .. }))
            .unwrap()
            .capture;
        super::recovery::archive_capture(&harness.outbox, &store, owner.fence(), &contract, &derived).await;
        if case == Case::Cleanup {
            let (mut source, records) = diagnostic_original_evidence(Case::CleanupEmpty);
            source_at(&mut source, 2);
            let cleared = harness.capture(Frame { source, records }).await;
            assert_eq!(harness.work(1).await, 1);
            super::recovery::archive_capture(&harness.outbox, &store, owner.fence(), &contract, &cleared).await;
            let latest = harness.outbox.status(20, None).await.unwrap().entries.into_iter().find(|entry| {
                matches!(&entry.request.selection, CollectionSelection::Diagnostics { parent, .. } if parent.capture == cleared)
            }).unwrap().capture;
            super::recovery::archive_capture(&harness.outbox, &store, owner.fence(), &contract, &latest).await;
        }
        let heads = store.diagnostic_heads(None, 10).await.unwrap();
        assert_eq!(heads.heads.len(), 1);
        let restored =
            crate::delivery::diagnostic_recovery::load_diagnostic_checkpoint(&store, &heads.heads[0]).await.unwrap();
        assert_eq!(restored.checkpoint(), &harness.checkpoint().await);
        let expected_calls = if case == Case::Cleanup { [2, 2, 2, 2] } else { [1, 1, 1, 1] };
        assert_eq!(harness.calls.counts(), expected_calls);
        assert!(store.pending_diagnostic_inputs(None, 10).await.unwrap().ranges.is_empty());
    }
}

#[test]
fn shared_failures_require_the_exact_original_instance_member_and_epoch_while_pool_rows_remain_explicitly_unqualified()
{
    let (source, rows) = diagnostic_original_evidence(Case::Memory);
    assert!(source.target.database().is_none());
    assert_eq!(
        diagnostic_original_input_qualification(Some(&source)),
        DiagnosticInputQualification::Qualified { source: DiagnosticSourceKind::MemoryFailures }
    );
    let original = observe(&source, &rows, None, "shared-instance-original");
    for episode in &original.checkpoint.episodes {
        assert_eq!(episode.scope.target, source.target);
        assert_eq!(episode.scope.measurement_scope, culprit_core::MeasurementScope::Server);
    }
    for case in ["target-only", "missing-epoch", "foreign-member", "foreign-target"] {
        let mut changed = source.clone();
        match case {
            "target-only" => changed.partition = SourcePartition::Target,
            "missing-epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { unreachable!() };
                *epoch = None;
            }
            "foreign-member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition else {
                    unreachable!()
                };
                member.native.id = "foreign-native-member".into();
            }
            _ => {
                let CollectionTarget::Shared(owner) = &mut changed.target else { unreachable!() };
                owner.native.id = "foreign-native-target".into();
            }
        }
        let qualification = diagnostic_original_input_qualification(Some(&changed));
        assert!(matches!(qualification, DiagnosticInputQualification::Ineligible { .. }), "{case}");
        assert!(!qualification.explanation().is_empty());
        assert!(crate::outbox::diagnostic_source_eligible(&changed, None).is_ok_and(|eligible| !eligible));
    }
    let (pool, pool_rows) = diagnostic_original_evidence(Case::MemoryAmbiguousPool);
    assert_eq!(pool_rows.len(), 2);
    assert_eq!(
        diagnostic_original_input_qualification(Some(&pool)),
        DiagnosticInputQualification::Ineligible { reason: DiagnosticInputIneligibility::SharedOwnershipUnavailable }
    );
    assert!(crate::outbox::diagnostic_source_eligible(&pool, None).is_ok_and(|eligible| !eligible));
    let mut partial = source.clone();
    let SourceObservation::Snapshot(snapshot) = &mut partial.observation else { unreachable!() };
    snapshot.sources[0] = StateSourceCoverage::observed(snapshot.sources[0].source.clone(), 2, false);
    let update = observe(&partial, &rows, None, "partial-shared-buffer");
    assert_eq!(update.checkpoint.episodes.len(), 2);
    assert!(update.checkpoint.episodes.iter().all(|episode| episode.lifecycle == DiagnosticLifecycle::Uncertain));
}

#[test]
fn a_shared_process_reset_or_changed_retained_slot_never_establishes_event_continuity_or_recovers_old_groups() {
    let (mut source, rows) = diagnostic_original_evidence(Case::Memory);
    let first = observe(&source, &rows, None, "before-shared-process-reset");
    source_at(&mut source, 2);
    let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { unreachable!() };
    *epoch = Some(digest(b"new original native process"));
    let reset = observe(&source, &rows, Some(&first.checkpoint), "after-shared-process-reset");
    assert_eq!(reset.checkpoint.episodes.len(), 4);
    let retained = reset
        .checkpoint
        .episodes
        .iter()
        .filter(|episode| first.checkpoint.episodes.iter().any(|before| before.id == episode.id))
        .count();
    assert_eq!(retained, 2);
    assert!(
        reset
            .checkpoint
            .episodes
            .iter()
            .all(|episode| { episode.lifecycle == DiagnosticLifecycle::Uncertain && episode.recovery.is_none() })
    );
    let mut rolled = rows;
    let TelemetryRecord::Operational(row) = &mut rolled[0] else { unreachable!() };
    let OperationalFact::MssqlOutOfMemory(failure) = &mut row.fact else { unreachable!() };
    failure.position = 2;
    row.key = failure.key().unwrap();
    source_at(&mut source, 4);
    let rollover = observe(&source, &rolled, Some(&reset.checkpoint), "different-retained-slot");
    assert_eq!(rollover.checkpoint.episodes.len(), 5);
    assert!(rollover.checkpoint.episodes.iter().all(|episode| episode.recovery.is_none()));
}

#[tokio::test]
async fn unresolved_history_stays_canonically_retained_without_invalid_local_or_archived_incident_work() {
    use crate::delivery::{contract::Contract, file_store::FileRecoveryStore, recovery::RecoveryStore};
    let (source, records) = diagnostic_original_evidence(Case::HistoryUnresolved);
    let archive = tempfile::tempdir().unwrap();
    let contract = Contract::new(
        "pipeline".into(),
        "shard".into(),
        "file:unresolved-history".into(),
        digest(b"history-original-schema"),
    )
    .unwrap();
    let store = FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let mut harness = Harness::with_recovery_store(Some(store.id())).await;
    harness.outbox.begin_remote_epoch(owner.fence(), 0).await.unwrap();
    let identity = harness.capture(Frame { source: source.clone(), records: records.clone() }).await;
    assert_eq!(harness.work(1).await, 0, "no unrepresentable original is pinned as diagnostic debt");
    let committed =
        super::recovery::archive_capture(&harness.outbox, &store, owner.fence(), &contract, &identity).await;
    let original = crate::delivery::recovery::reopen(&store, &committed.manifest().range_id).await.unwrap();
    assert!(original.manifest().reference_coverage.integrity_complete);
    assert!(!original.manifest().reference_coverage.complete);
    assert!(store.pending_diagnostic_inputs(None, 10).await.unwrap().ranges.is_empty());
    assert!(
        crate::delivery::diagnostic_recovery::load_diagnostic_input(&store, &committed.manifest().range_id)
            .await
            .is_err()
    );
    let exported = ExportedRecord::new(&source, &identity, records[0].clone()).unwrap();
    assert!(exported.record_id.is_none());
    let mut expected = records[0].clone();
    let TelemetryRecord::QueryRuntime(runtime) = &mut expected else { unreachable!() };
    // The original instants use the canonical export notation; every other native value stays exact.
    runtime.first_execution_at = Some("2020-01-01T06:01:00.000000000Z".into());
    runtime.last_execution_at = Some("2020-01-01T06:59:00.000000000Z".into());
    assert_eq!(exported.record, expected);
    assert!(DiagnosticEvidenceRef::from_export(&source, &digest(b"original-manifest"), &exported, 0).is_err());
    assert_eq!(harness.calls.counts(), [1, 1, 1, 1]);
}

#[test]
fn complete_original_definition_identity_accepts_unavailable_sql_but_rejects_missing_or_tampered_coverage() {
    let (source, rows) = diagnostic_original_evidence(Case::HistoryException);
    let mut references = crate::collection_references::ObservationReferences::default();
    for row in &rows {
        references.record(row);
    }
    let complete = references.coverage(true, Some(&source));
    assert!(complete.complete);
    assert!(rows.iter().any(|row| matches!(row, TelemetryRecord::PlanDefinition(plan)
        if matches!(plan.plan, DefinitionPayload::Unavailable { .. }))));
    assert!(crate::outbox::diagnostic_manifest_eligible(&source, None, &complete).unwrap());
    let (unresolved, rows) = diagnostic_original_evidence(Case::HistoryUnresolved);
    let mut references = crate::collection_references::ObservationReferences::default();
    for row in &rows {
        references.record(row);
    }
    let mut incomplete = references.coverage(true, Some(&unresolved));
    assert!(incomplete.integrity_complete && !incomplete.complete);
    assert!(!crate::outbox::diagnostic_manifest_eligible(&unresolved, None, &incomplete).unwrap());
    incomplete.complete = true;
    assert!(!crate::outbox::diagnostic_manifest_eligible(&unresolved, None, &incomplete).unwrap());
}
