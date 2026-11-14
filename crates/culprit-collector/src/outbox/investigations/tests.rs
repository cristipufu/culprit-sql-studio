mod events;
mod focus;
mod retirement;
use super::*;
use crate::{
    collection_files::ObservationFiles,
    delivery::{recovery::*, sink::*},
};
use std::sync::Arc;

fn activity(id: &str) -> ActivityObservation {
    ActivityObservation {
        key: ActivityKey {
            session: NativeActivityIdentity { id: id.into(), generation: "login-1".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "request-1".into() }),
            transaction: None,
        },
        scope: ActivityScopeBasis::RequestDatabase,
        state: "suspended".into(),
        idle: false,
        database: Some("db".into()),
        command: Some("SELECT".into()),
        elapsed_ms: Some("90000".into()),
        last_request_age_ms: None,
        open_transactions: Some(0),
        transaction_age_ms: None,
        session_counters: Default::default(),
        request_counters: Some(Default::default()),
        wait: None,
        query_hash: None,
        statement_identity: None,
        client: None,
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        blocking_status: ActivityBlockingStatus::Known,
        waiting_on_lock: None,
    }
}
fn baseline(files: &mut Files, rows: Vec<ActivityObservation>, complete: bool, second: u32) -> CaptureIdentity {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: CollectionBudget { page_bytes: 128 * 1024, total_bytes: 4 * 1024 * 1024, ..Default::default() },
    };
    let reservation = files.reserve("fixture", &request).unwrap();
    let identity = reservation.entry.identity.clone();
    files.journal.source_work(&reservation.entry, SourceWorkState::Active).unwrap();
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        source: "activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            epoch: Some("native-epoch".into()),
            observed_from: format!("2026-11-14T08:00:{second:02}Z"),
            observed_until: format!("2026-11-14T08:00:{:02}Z", second + 1),
            at: format!("2026-11-14T08:00:{second:02}Z"),
            scope: culprit_core::MeasurementScope::Database,
            native_rows: rows.len() as u32,
            observations: rows.len() as u32,
            coverage: StateSourceCoverage::observed("activity".into(), rows.len() as u32, complete),
        }),
    };
    let mut writer = ObservationFiles::create(&reservation.directory, request, identity.clone(), None).unwrap();
    writer.describe(source).unwrap();
    writer.begin_page(0).unwrap();
    assert!(writer.write_page(0, &rows.into_iter().map(TelemetryRecord::from).collect::<Vec<_>>()).unwrap().is_none());
    writer.seal(0).unwrap();
    writer.confirm_source(SourceCompletion::Verified).unwrap();
    assert_eq!(writer.finish(1).unwrap().status, CollectionStatus::Complete);
    files.journal.source_work(&reservation.entry, SourceWorkState::Quiescent).unwrap();
    files.finish(reservation).unwrap();
    identity
}
fn activity_signal(reference: &DiagnosticEvidenceRef) -> DiagnosticSignal {
    let NativeRecordIdentity::Activity(key) = &reference.native_key else { panic!() };
    DiagnosticSignal::Activity {
        key: key.clone(),
        condition: ActivityCondition::LongRunning,
        policy: ActivityPolicy::default(),
    }
}
fn start(input: &DiagnosticInput) -> DiagnosticUpdate {
    let row = input.rows.iter().find(|row| matches!(row.row.record, TelemetryRecord::Activity(_))).unwrap();
    let episode = DiagnosticEpisode::start(
        DiagnosticScope::from_source(&input.source).unwrap(),
        activity_signal(&row.reference),
        input.source.observation.activity().unwrap().at.clone(),
        row.reference.clone(),
    )
    .unwrap();
    update(input, vec![episode], vec![None], "initial")
}
fn update(
    input: &DiagnosticInput,
    episodes: Vec<DiagnosticEpisode>,
    previous: Vec<Option<String>>,
    step: &str,
) -> DiagnosticUpdate {
    let at = "2026-11-14T08:03:00Z".to_owned();
    let revisions = episodes
        .iter()
        .cloned()
        .zip(previous)
        .map(|(episode, previous_revision)| {
            let mut inputs = vec![episode.first.clone()];
            for reference in [&episode.latest].into_iter().chain(episode.peak.iter()) {
                if !inputs.contains(reference) {
                    inputs.push(reference.clone());
                }
            }
            let mut revision = DiagnosticRevision {
                id: String::new(),
                controller_version: 1,
                episode,
                previous_revision,
                investigation: None,
                inputs,
                analyzed_at: at.clone(),
            };
            revision.id = revision.expected_id().unwrap();
            revision
        })
        .collect::<Vec<_>>();
    DiagnosticUpdate {
        step_id: digest(step.as_bytes()),
        analyzed_at: at,
        revisions,
        checkpoint: DiagnosticCheckpoint {
            controller_version: 1,
            target: input.source.target.clone(),
            input: input.parent.clone(),
            episodes: episodes.into_iter().filter(DiagnosticEpisode::retained_in_checkpoint).collect(),
        },
    }
}
#[test]
fn only_exact_committed_analysis_acknowledgement_establishes_input_consumption() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![activity("42")], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    assert_eq!(files.diagnostic_input_state(&input.parent, 1).unwrap(), DiagnosticInputState::Pending);
    let mut wrong = input.parent.clone();
    wrong.manifest_sha256 = digest(b"another immutable manifest");
    assert!(files.diagnostic_input_state(&wrong, 1).is_err());
    assert!(files.diagnostic_input_state(&input.parent, 2).is_err());
    let mut unknown = input.parent.clone();
    unknown.capture.observation_id = "never-cached-capture".into();
    assert_eq!(
        files.diagnostic_input_state(&unknown, 1).unwrap(),
        DiagnosticInputState::Missing,
        "absence is never an analysis acknowledgement"
    );
    files.commit_diagnostics(input.clone(), start(&input)).unwrap();
    assert_eq!(files.diagnostic_input_state(&input.parent, 1).unwrap(), DiagnosticInputState::Consumed);
    assert!(files.diagnostic_input_state(&wrong, 1).is_err());
    files
        .journal
        .conn
        .execute(
            "UPDATE diagnostic_inputs SET source_json='{}' WHERE observation_id=?1",
            [&input.parent.capture.observation_id],
        )
        .unwrap();
    assert!(
        files.diagnostic_input_state(&input.parent, 1).is_err(),
        "even an acknowledged row must retain a valid matching source"
    );
}

#[test]
fn ready_input_and_derived_revision_acknowledgement_are_durable_and_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut files = Files::create(&path, OutboxLimits::default()).unwrap();
    let native = baseline(&mut files, vec![activity("42")], true, 0);
    assert!(super::journal::is_pinned(&files.journal.conn, &native.observation_id).unwrap());
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let update = start(&input);
    let expected = update.checkpoint.clone();
    let derived = files.commit_diagnostics(input.clone(), update.clone()).unwrap();
    assert_eq!(files.journal.capture(&derived.observation_id).unwrap().source_work, SourceWorkState::Quiescent);
    assert_eq!(files.journal.debts().unwrap(), 0);
    assert!(files.next_diagnostic_input(1).unwrap().is_none());
    assert_eq!(
        files.commit_diagnostics(input, update).unwrap(),
        derived,
        "lost acknowledgement cannot allocate another revision"
    );
    drop(files);
    let mut files = Files::open(&path).unwrap();
    files.recover().unwrap();
    assert_eq!(files.diagnostic_input(&native.observation_id, 1).unwrap().checkpoint, Some(expected));
    assert!(files.next_diagnostic_input(1).unwrap().is_none());
}
#[test]
fn a_partial_or_empty_later_population_cannot_silently_drop_old_episodes() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![activity("42")], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    files.commit_diagnostics(input.clone(), start(&input)).unwrap();
    baseline(&mut files, vec![], false, 10);
    let partial = files.next_diagnostic_input(1).unwrap().unwrap();
    let mut forged = DiagnosticUpdate {
        step_id: digest(b"drop"),
        analyzed_at: "2026-11-14T08:03:00Z".into(),
        revisions: vec![],
        checkpoint: DiagnosticCheckpoint {
            controller_version: 1,
            target: partial.source.target.clone(),
            input: partial.parent.clone(),
            episodes: vec![],
        },
    };
    assert!(files.commit_diagnostics(partial.clone(), forged.clone()).is_err());
    assert!(files.next_diagnostic_input(1).unwrap().is_some());
    let mut resolved = partial.checkpoint.as_ref().unwrap().episodes[0].clone();
    resolved.revision = "2".into();
    resolved.lifecycle = DiagnosticLifecycle::Resolved;
    resolved.recovery = Some(DiagnosticRecoveryProof::CompleteScope { capture: partial.parent.clone() });
    forged = update(&partial, vec![resolved], vec![Some("1".into())], "false-recovery");
    assert!(files.commit_diagnostics(partial, forged).is_err(), "unknown native visibility cannot prove resolution");
}
#[test]
fn a_crash_after_local_sealing_replays_cas_and_ack_without_creating_native_debt() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut files = Files::create(&path, OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![activity("42")], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    files
        .journal
        .conn
        .execute_batch(
            "CREATE TRIGGER injected BEFORE INSERT ON diagnostic_heads BEGIN SELECT RAISE(ABORT,'injected'); END",
        )
        .unwrap();
    assert!(files.commit_diagnostics(input.clone(), start(&input)).is_err());
    assert_eq!(files.journal.unfinished().unwrap().len(), 1);
    assert_eq!(files.journal.debts().unwrap(), 0);
    files.journal.conn.execute_batch("DROP TRIGGER injected").unwrap();
    drop(files);
    let mut files = Files::open(&path).unwrap();
    let recovery = files.recover().unwrap();
    assert_eq!(recovery.committed, 1);
    assert_eq!(recovery.source_debts, 0);
    assert!(files.next_diagnostic_input(1).unwrap().is_none());
}
#[test]
fn quota_backpressure_keeps_the_native_input_pending_and_pinned() {
    let root = tempfile::tempdir().unwrap();
    let limits = OutboxLimits { max_bytes: 8 * 1024 * 1024, ..Default::default() };
    let mut files = Files::create(&root.path().join("outbox"), limits).unwrap();
    let native = baseline(&mut files, vec![activity("42")], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    assert!(files.commit_diagnostics(input.clone(), start(&input)).is_err());
    assert!(files.next_diagnostic_input(1).unwrap().is_some());
    assert_eq!(files.journal.debts().unwrap(), 0);
    assert!(super::journal::is_pinned(&files.journal.conn, &native.observation_id).unwrap());
}
#[tokio::test]
async fn publication_before_analysis_survives_volume_loss_and_preserves_the_full_pending_working_set() {
    let root = tempfile::tempdir().unwrap();
    let contract = crate::delivery::contract::Contract::new(
        "pipeline".into(),
        "shard-0".into(),
        "file:warehouse".into(),
        digest(b"schema"),
    )
    .unwrap();
    let store = Arc::new(
        crate::delivery::file_store::FileRecoveryStore::create(&root.path().join("recovery"), contract.clone())
            .unwrap(),
    );
    let owner = store.acquire().await.unwrap();
    let sink = crate::delivery::file_sink::FileTelemetrySink::create(
        &root.path().join("warehouse"),
        contract.clone(),
        store.clone(),
    )
    .unwrap();
    let mut files = Files::create_remote(&root.path().join("first"), OutboxLimits::default(), store.id()).unwrap();
    files.recover().unwrap();
    files.begin_remote_epoch(owner.fence(), 0).unwrap();
    let native = baseline(&mut files, vec![activity("42"), activity("43")], true, 0);
    let prepared = files.prepare_delivery(&native.observation_id, contract.clone(), vec![]).unwrap();
    let committed = stage_and_commit(store.as_ref(), owner.fence(), None, &prepared).await.unwrap();
    let original = files.next_diagnostic_input(1).unwrap().unwrap();
    let reference = original.rows[0].reference.clone();
    crate::delivery::diagnostic_recovery::verify_reference(committed.manifest(), &reference).unwrap();
    let mut wrong = reference.clone();
    wrong.row_ordinal = 1;
    assert!(crate::delivery::diagnostic_recovery::verify_reference(committed.manifest(), &wrong).is_err());
    wrong = reference.clone();
    wrong.native_key = original.rows[1].reference.native_key.clone();
    assert!(crate::delivery::diagnostic_recovery::verify_reference(committed.manifest(), &wrong).is_err());
    wrong = reference;
    wrong.manifest_sha256 = digest(b"different raw manifest");
    assert!(crate::delivery::diagnostic_recovery::verify_reference(committed.manifest(), &wrong).is_err());

    let attempt = DeliveryAttempt::new(1).unwrap();
    dispatch_range(&sink, &committed, &attempt).await.unwrap();
    let verified = verify(&sink, &committed, VerificationBudget::default()).await.unwrap();
    sink.publish(&verified, &attempt).await.unwrap();
    let published = read_publication(&sink, &committed).await.unwrap();
    store.retire(owner.fence(), &published).await.unwrap();
    assert_eq!(
        store.pending_diagnostic_inputs(None, 10).await.unwrap().ranges.len(),
        1,
        "publication cannot erase an uncheckpointed transient baseline"
    );
    drop(files);
    let mut cold = Files::create_remote(&root.path().join("cold"), OutboxLimits::default(), store.id()).unwrap();
    cold.recover().unwrap();
    let recovered =
        crate::delivery::diagnostic_recovery::load_diagnostic_input(store.as_ref(), &committed.manifest().range_id)
            .await
            .unwrap();
    let (range, rows) = recovered.into_parts();
    cold.restore_diagnostic_input(RestoredInput {
        store_id: store.id().into(),
        parent: crate::delivery::diagnostic_recovery::capture_reference(range.manifest()).unwrap(),
        source: range.manifest().source.clone(),
        connection: "fixture".into(),
        rows,
    })
    .unwrap();
    cold.begin_remote_epoch(owner.fence(), 1).unwrap();
    let input = cold.next_diagnostic_input(1).unwrap().unwrap();
    let mut initial = start(&input);
    let second = DiagnosticEpisode::start(
        DiagnosticScope::from_source(&input.source).unwrap(),
        activity_signal(&input.rows[1].reference),
        input.source.observation.activity().unwrap().at.clone(),
        input.rows[1].reference.clone(),
    )
    .unwrap();
    initial.revisions.extend(update(&input, vec![second.clone()], vec![None], "second").revisions);
    initial.checkpoint.episodes.push(second);
    let expected = initial.checkpoint.clone();
    let derived = cold.commit_diagnostics(input, initial).unwrap();
    let prepared = cold.prepare_delivery(&derived.observation_id, contract, vec![]).unwrap();
    stage_and_commit(store.as_ref(), owner.fence(), Some(&committed.manifest().range_id), &prepared).await.unwrap();
    assert!(store.pending_diagnostic_inputs(None, 10).await.unwrap().ranges.is_empty());
    let pending = store.pending_ranges(None, 10, RecoveryOrder::OldestFirst).await.unwrap();
    assert_eq!(pending.ranges.len(), 1);
    assert_eq!(pending.ranges[0].capture, derived, "only the derived publication remains pending");
    let heads = store.diagnostic_heads(None, 10).await.unwrap();
    assert_eq!(heads.heads.len(), 1);
    let recovered = crate::delivery::diagnostic_recovery::load_diagnostic_checkpoint(store.as_ref(), &heads.heads[0])
        .await
        .unwrap();
    assert_eq!(recovered.checkpoint(), &expected);
    let mut cold_again =
        Files::create_remote(&root.path().join("cold-again"), OutboxLimits::default(), store.id()).unwrap();
    cold_again.recover().unwrap();
    cold_again.restore_diagnostic_checkpoint(recovered).unwrap();
    let count: i64 =
        cold_again.journal.conn.query_row("SELECT count(*) FROM diagnostic_episodes", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 2);
}

#[test]
fn recovery_before_enrichment_retains_pending_analysis_and_a_later_result_does_not_reopen_the_episode() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![activity("42")], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    files.commit_diagnostics(input.clone(), start(&input)).unwrap();
    let empty = baseline(&mut files, vec![], true, 10);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let mut episode = input.checkpoint.as_ref().unwrap().episodes[0].clone();
    let last_seen = episode.last_seen_at.clone();
    episode.revision = "2".into();
    episode.lifecycle = DiagnosticLifecycle::Resolved;
    episode.recovery = Some(DiagnosticRecoveryProof::CompleteScope { capture: input.parent.clone() });
    files
        .commit_diagnostics(input.clone(), update(&input, vec![episode], vec![Some("1".into())], "resolved-pending"))
        .unwrap();
    let input = files.diagnostic_input(&empty.observation_id, 1).unwrap();
    let mut episode = input.checkpoint.as_ref().unwrap().episodes[0].clone();
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Pending);
    assert_eq!(episode.last_seen_at, last_seen);
    episode.revision = "3".into();
    episode.analysis = DiagnosticAnalysisStatus::Unresolved;
    episode.issue = Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
    files
        .commit_diagnostics(input.clone(), update(&input, vec![episode], vec![Some("2".into())], "no-more-evidence"))
        .unwrap();
    assert!(files.diagnostic_input(&empty.observation_id, 1).unwrap().checkpoint.unwrap().episodes.is_empty());
}
#[test]
fn stale_head_cas_cannot_replace_a_newer_episode_working_set() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![activity("42")], true, 0);
    let stale = files.next_diagnostic_input(1).unwrap().unwrap();
    files.commit_diagnostics(stale.clone(), start(&stale)).unwrap();
    let mut later = start(&stale);
    later.step_id = digest(b"different-step-same-stale-head");
    assert!(files.commit_diagnostics(stale, later).is_err());
    assert_eq!(files.journal.status(10, None).unwrap().captures, 2);
    assert_eq!(files.journal.debts().unwrap(), 0);
}
#[test]
fn a_large_checkpoint_is_packed_as_bounded_complete_parts_instead_of_one_oversized_record() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![activity("42")], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let original = start(&input).checkpoint.episodes.remove(0);
    let mut checkpoint = DiagnosticCheckpoint {
        controller_version: 1,
        target: input.source.target.clone(),
        input: input.parent,
        episodes: vec![],
    };
    for number in 0..100 {
        let mut episode = original.clone();
        episode.signal = DiagnosticSignal::PageWait {
            key: episode.signal.activity_key().clone(),
            statement_identity: None,
            resource: culprit_core::ObservedWaitResource {
                kind: culprit_core::WaitResourceKind::MemoryPage,
                native: NativeIdentity::new(format!("native-resource-{number}")),
            },
        };
        episode.id = episode.expected_id().unwrap();
        checkpoint.episodes.push(episode);
    }
    let budget = CollectionBudget { page_bytes: 16 * 1024, ..Default::default() };
    let parts = super::writer::checkpoint_parts(&checkpoint, &budget).unwrap();
    assert!(parts.len() > 1);
    assert!(parts.iter().all(|part| serde_json::to_vec(part).unwrap().len() < budget.page_bytes as usize));
    assert_eq!(DiagnosticCheckpointPart::assemble(parts).unwrap(), checkpoint);
}

#[test]
fn offline_diagnostic_authority_does_not_clear_native_debt_or_enable_source_reservations() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let store = digest(b"recovery-store");
    let first = Fence::issued(1, "first".into(), store.clone()).unwrap();
    let mut files = Files::create_remote(&path, OutboxLimits::default(), &store).unwrap();
    files.recover().unwrap();
    files.begin_remote_epoch(&first, 0).unwrap();
    baseline(&mut files, vec![activity("42")], true, 0);
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: Default::default(),
    };
    let uncertain = files.reserve("fixture", &request).unwrap();
    files.journal.source_work(&uncertain.entry, SourceWorkState::Active).unwrap();
    files
        .journal
        .fail(&uncertain.entry, OutboxCaptureState::Interrupted, None, super::super::CAPTURE_ACCOUNTING)
        .unwrap();
    drop(files);
    let mut files = Files::open(&path).unwrap();
    files.recover().unwrap();
    let fence = Fence::issued(2, "next".into(), store).unwrap();
    assert!(files.begin_remote_epoch(&fence, 2).is_err());
    files.begin_diagnostic_epoch(&fence, 2).unwrap();
    assert!(files.reserve("fixture", &request).is_err());
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let derived = files.commit_diagnostics(input.clone(), start(&input)).unwrap();
    assert_eq!(files.journal.capture(&derived.observation_id).unwrap().source_work, SourceWorkState::Quiescent);
    assert_eq!(files.journal.debts().unwrap(), 1);
    assert!(files.begin_remote_epoch(&fence, 3).is_err());
    assert!(files.reserve("fixture", &request).is_err());
}

#[test]
fn a_complete_inventory_cannot_resolve_a_present_actor_whose_required_measurements_are_unknown() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![activity("42")], true, 0);
    let first = files.next_diagnostic_input(1).unwrap().unwrap();
    files.commit_diagnostics(first.clone(), start(&first)).unwrap();
    let mut unknown = activity("42");
    unknown.elapsed_ms = None;
    unknown.blocking_status = ActivityBlockingStatus::Unknown;
    baseline(&mut files, vec![unknown], true, 10);
    let current = files.next_diagnostic_input(1).unwrap().unwrap();
    let mut episode = current.checkpoint.as_ref().unwrap().episodes[0].clone();
    episode.lifecycle = DiagnosticLifecycle::Resolved;
    episode.recovery = Some(DiagnosticRecoveryProof::CompleteScope { capture: current.parent.clone() });
    episode.revision = "2".into();
    assert!(
        files
            .commit_diagnostics(
                current.clone(),
                update(&current, vec![episode], vec![Some("1".into())], "false-present-recovery")
            )
            .is_err()
    );
    assert!(files.next_diagnostic_input(1).unwrap().is_some());
    assert_eq!(files.journal.status(10, None).unwrap().captures, 3);
}

#[tokio::test]
async fn caching_unknown_authenticated_history_after_epoch_binding_does_not_enqueue_or_acknowledge_pending_work() {
    let root = tempfile::tempdir().unwrap();
    let contract = crate::delivery::contract::Contract::new(
        "pipeline".into(),
        "shard-0".into(),
        "file:warehouse".into(),
        digest(b"schema"),
    )
    .unwrap();
    let store = crate::delivery::file_store::FileRecoveryStore::create(&root.path().join("recovery"), contract.clone())
        .unwrap();
    let owner = store.acquire().await.unwrap();
    let mut original =
        Files::create_remote(&root.path().join("original"), OutboxLimits::default(), store.id()).unwrap();
    original.recover().unwrap();
    original.begin_remote_epoch(owner.fence(), 0).unwrap();
    let first = baseline(&mut original, vec![activity("42")], true, 0);
    let prepared = original.prepare_delivery(&first.observation_id, contract.clone(), vec![]).unwrap();
    let first_commit = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    let initial_input = original.next_diagnostic_input(1).unwrap().unwrap();
    let initial = original.commit_diagnostics(initial_input.clone(), start(&initial_input)).unwrap();
    let prepared = original.prepare_delivery(&initial.observation_id, contract.clone(), vec![]).unwrap();
    let initial_commit =
        stage_and_commit(&store, owner.fence(), Some(&first_commit.manifest().range_id), &prepared).await.unwrap();
    let second = baseline(&mut original, vec![activity("43")], true, 10);
    let prepared = original.prepare_delivery(&second.observation_id, contract, vec![]).unwrap();
    let second_commit =
        stage_and_commit(&store, owner.fence(), Some(&initial_commit.manifest().range_id), &prepared).await.unwrap();
    let mut cold = Files::create_remote(&root.path().join("cold"), OutboxLimits::default(), store.id()).unwrap();
    cold.recover().unwrap();
    let pending =
        crate::delivery::diagnostic_recovery::load_diagnostic_input(&store, &second_commit.manifest().range_id)
            .await
            .unwrap();
    let (range, rows) = pending.into_parts();
    cold.restore_diagnostic_input(RestoredInput {
        store_id: store.id().into(),
        parent: crate::delivery::diagnostic_recovery::capture_reference(range.manifest()).unwrap(),
        source: range.manifest().source.clone(),
        connection: "pending".into(),
        rows,
    })
    .unwrap();
    cold.begin_diagnostic_epoch(owner.fence(), second.revision.as_ref().unwrap().sequence.parse().unwrap()).unwrap();
    let recovered =
        crate::delivery::diagnostic_recovery::load_diagnostic_input(&store, &first_commit.manifest().range_id)
            .await
            .unwrap();
    let (range, rows) = recovered.into_parts();
    cold.cache_diagnostic_evidence(RestoredInput {
        store_id: store.id().into(),
        parent: crate::delivery::diagnostic_recovery::capture_reference(range.manifest()).unwrap(),
        source: range.manifest().source.clone(),
        connection: "historical".into(),
        rows,
    })
    .unwrap();
    assert_eq!(cold.next_diagnostic_input(1).unwrap().unwrap().parent.capture, second);
    let historical = cold.diagnostic_input(&first.observation_id, 1).unwrap();
    assert_eq!(historical.rows.len(), 1);
    let (_, resolved) = cold.diagnostic_reference(&historical.rows[0].reference).unwrap();
    assert_eq!(resolved, historical.rows[0].row);
    let count: i64 = cold
        .journal
        .conn
        .query_row("SELECT count(*) FROM diagnostic_inputs WHERE acknowledged=0", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(cold.journal.debts().unwrap(), 0);
    assert!(!cold.native_epoch_enabled);
}
