use super::*;

pub(super) fn page_activity() -> ActivityObservation {
    let mut row = activity("42");
    let resource = culprit_core::ObservedWaitResource {
        kind: culprit_core::WaitResourceKind::MemoryPage,
        native: NativeIdentity::new("5:1:42"),
    };
    row.statement_identity = Some(NativeIdentity::new("exact-statement").qualified("offset", "0"));
    row.wait = Some(ActivityWait {
        identified_resources: vec![resource],
        native_type: Some("PAGELATCH_EX".into()),
        elapsed_ms: Some("1000".into()),
        owners: vec![],
        resources: None,
    });
    row
}
pub(super) fn focus(input: &DiagnosticInput) -> ActivityWaitFocus {
    focus_at(input, 0)
}
fn focus_at(input: &DiagnosticInput, ordinal: usize) -> ActivityWaitFocus {
    let row = &input.rows[ordinal];
    let TelemetryRecord::Activity(original) = &row.row.record else { panic!() };
    let native = input.source.observation.activity().unwrap();
    ActivityWaitFocus {
        parent: row.reference.clone(),
        target: input.source.target.clone(),
        source: input.source.source.clone(),
        epoch: native.epoch.clone().unwrap(),
        original_observed_at: native.at.clone(),
        original: original.as_ref().clone(),
        statement_identity: original.statement_identity.clone().unwrap(),
        resource: original.wait.as_ref().unwrap().identified_resources[0].clone(),
    }
}
pub(super) fn page_episode(input: &DiagnosticInput, focus: &ActivityWaitFocus) -> DiagnosticEpisode {
    DiagnosticEpisode::start(
        DiagnosticScope::from_source(&input.source).unwrap(),
        DiagnosticSignal::PageWait {
            key: focus.original.key.clone(),
            statement_identity: Some(focus.statement_identity.clone()),
            resource: focus.resource.clone(),
        },
        focus.original_observed_at.clone(),
        focus.parent.clone(),
    )
    .unwrap()
}
fn request(focus: ActivityWaitFocus) -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::FocusedPageWait { focus: Box::new(focus) },
        budget: Default::default(),
    }
}
fn pin(files: &Files, owner: &str) -> bool {
    files
        .journal
        .conn
        .query_row("SELECT EXISTS(SELECT 1 FROM diagnostic_pins WHERE owner=?1)", [owner], |row| row.get(0))
        .unwrap()
}

pub(super) fn admitted_intent(input: &DiagnosticInput) -> DiagnosticInvestigationIntent {
    let question = focus(input);
    let episode = page_episode(input, &question);
    let mut intent = crate::analytical::test_support::investigation_fixture().intent;
    intent.focus = question;
    intent.episode_id = episode.id;
    intent.episode_revision = episode.revision;
    intent.qualification.scope = episode.scope;
    assert!(intent.is_valid(), "the cost approval is synthetic; this test grants no native authority");
    intent
}

#[test]
fn failed_owned_attempt_retains_exact_intent_before_description_and_rejects_foreign_owner_or_budget() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut files = Files::create(&path, OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![page_activity()], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let mut intent = admitted_intent(&input);
    intent.qualification.read_metrics.templates[0].client_elapsed_ms = 0.0;
    let owner = DiagnosticInvestigationLineage::from_intent(&intent).unwrap();
    let mut requested = request(intent.focus.clone());
    requested.budget = intent.budget.clone();
    let reservation = files.reserve("fixture", &requested).unwrap();
    let identity = reservation.entry.identity.clone();
    let writer =
        ObservationFiles::create(&reservation.directory, requested.clone(), identity.clone(), Some(owner.clone()))
            .unwrap();
    let manifest_path = reservation.directory.join("manifest.json");
    let initial = files.attempt_receipt(&identity, Some(&owner)).unwrap();
    assert_eq!(initial.manifest_sha256, Some(digest(&std::fs::read(&manifest_path).unwrap())));
    assert!(writer.manifest.description.is_none());
    assert_eq!(writer.manifest.investigation_intent().unwrap(), Some(intent.clone()));
    let mut foreign = intent.clone();
    foreign.episode_id = digest(b"a different episode with the same original question");
    let foreign = DiagnosticInvestigationLineage::from_intent(&foreign).unwrap();
    assert!(foreign.matches_request(&requested));
    assert!(files.attempt_receipt(&identity, Some(&foreign)).is_err());
    let mut different_encoding = intent.clone();
    different_encoding.qualification.read_metrics.templates[0].client_elapsed_ms = -0.0;
    assert_eq!(different_encoding, intent, "numeric equality cannot establish immutable owner identity");
    let different_encoding = DiagnosticInvestigationLineage::from_intent(&different_encoding).unwrap();
    assert!(files.attempt_receipt(&identity, Some(&different_encoding)).is_err());
    assert!(files.attempt_receipt(&identity, None).is_err());
    let mut stopped = writer.finish(0).unwrap();
    assert!(matches!(stopped.status, CollectionStatus::Incomplete(_)));
    let final_bytes = std::fs::read(&manifest_path).unwrap();
    files.finish(reservation).unwrap();
    drop(files);
    let files = Files::open(&path).unwrap();
    assert_eq!(files.attempt_receipt(&identity, Some(&owner)).unwrap().manifest_sha256, Some(digest(&final_bytes)));
    stopped.request.budget.total_timeout_ms += 1;
    assert!(stopped.request.validate().is_ok());
    std::fs::write(&manifest_path, serde_json::to_vec(&stopped).unwrap()).unwrap();
    assert!(files.attempt_receipt(&identity, Some(&owner)).is_err());
}

#[tokio::test]
async fn complete_recovery_range_preserves_owner_without_claiming_incomplete_native_work_complete() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![page_activity()], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let intent = admitted_intent(&input);
    let owner = DiagnosticInvestigationLineage::from_intent(&intent).unwrap();
    let mut requested = request(intent.focus.clone());
    requested.budget = intent.budget.clone();
    let reservation = files.reserve("fixture", &requested).unwrap();
    let identity = reservation.entry.identity.clone();
    let directory = reservation.directory.clone();
    let result = ActivityWaitFocusResult {
        focus: intent.focus.clone(),
        observed_from: "2026-11-14T08:00:02Z".into(),
        observed_until: "2026-11-14T08:00:03Z".into(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::Missing,
        wait: PageWaitRelation::Unqualified,
    };
    let source = CollectionDescription {
        target: result.focus.target.clone(),
        source: "focused-page-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedPageWait {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitTargets,
                observed_from: result.observed_from.clone(),
                observed_until: result.observed_until.clone(),
                records: 0,
                sources: vec![StateSourceCoverage::collected("page wait targets".into(), 0)],
            },
            result: Box::new(result),
        },
    };
    let mut writer = ObservationFiles::create(&directory, requested, identity.clone(), Some(owner.clone())).unwrap();
    let contract = crate::delivery::contract::Contract::new(
        "pipeline".into(),
        "shard-0".into(),
        "file:test".into(),
        digest(b"schema"),
    )
    .unwrap();
    assert!(
        crate::delivery::evidence::PreparedRange::from_validated(
            &directory,
            &writer.manifest,
            contract.clone(),
            vec![],
        )
        .is_err()
    );
    files.journal.source_work(&reservation.entry, SourceWorkState::Active).unwrap();
    writer.describe(source).unwrap();
    writer.seal(0).unwrap();
    writer.confirm_source(SourceCompletion::Verified).unwrap();
    let manifest = writer.finish(1).unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    files.journal.source_work(&reservation.entry, SourceWorkState::Quiescent).unwrap();
    files.finish(reservation).unwrap();
    let prepared = files.prepare_delivery(&identity.observation_id, contract.clone(), vec![]).unwrap();
    assert_eq!(prepared.manifest().investigation, Some(owner.clone()));
    let store = crate::delivery::file_store::FileRecoveryStore::create(&root.path().join("archive"), contract).unwrap();
    let lease = store.acquire().await.unwrap();
    let archived = crate::delivery::recovery::stage_and_commit(&store, lease.fence(), None, &prepared).await.unwrap();
    let reopened = crate::delivery::recovery::reopen(&store, &archived.manifest().range_id).await.unwrap();
    let retained = reopened.manifest().investigation.as_ref().unwrap();
    let SourceObservation::FocusedPageWait { result, .. } = &reopened.manifest().source.observation else { panic!() };
    assert_eq!(retained.reconstruct(&result.focus).unwrap().key().unwrap(), intent.key().unwrap());
    let mut wrong_source = reopened.manifest().clone();
    wrong_source.source.target =
        culprit_core::DatabaseIdentity::server(culprit_core::Engine::Mssql, "different", "foreign.invalid", 1433, None)
            .into();
    assert!(wrong_source.validate().is_err());
    let mut wrong_budget = manifest;
    wrong_budget.request.budget.total_timeout_ms += 1;
    assert!(
        crate::delivery::evidence::PreparedRange::from_validated(
            &directory,
            &wrong_budget,
            reopened.manifest().contract.clone(),
            vec![],
        )
        .is_err()
    );
}

#[test]
fn queued_investigation_parent_is_authenticated_idempotent_and_preserved_across_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut files = Files::create(&path, OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![page_activity()], true, 0);
    let first = files.next_diagnostic_input(1).unwrap().unwrap();
    let question = focus(&first);
    let key = digest(b"immutable-investigation-intent");
    let mut substituted = question.clone();
    substituted.original.elapsed_ms = Some("90001".into());
    assert!(files.pin_investigation_parent(&key, &substituted).is_err());
    assert!(files.investigation_parent_keys().unwrap().is_empty());
    files.pin_investigation_parent(&key, &question).unwrap();
    files.pin_investigation_parent(&key, &question).unwrap();
    assert_eq!(files.investigation_parent_keys().unwrap().as_slice(), std::slice::from_ref(&key));

    let newer = baseline(&mut files, vec![page_activity()], true, 10);
    let newer = files.diagnostic_input(&newer.observation_id, 1).unwrap();
    assert!(
        files.pin_investigation_parent(&key, &focus(&newer)).is_err(),
        "an immutable queued key cannot silently change its original parent"
    );
    drop(files);
    let mut files = Files::open(&path).unwrap();
    files.recover().unwrap();
    assert_eq!(files.investigation_parent_keys().unwrap().as_slice(), std::slice::from_ref(&key));
    files.pin_investigation_parent(&key, &question).unwrap();
    files.release_investigation_parent(&key).unwrap();
    files.release_investigation_parent(&key).unwrap();
    assert!(files.investigation_parent_keys().unwrap().is_empty());
    assert!(
        super::super::journal::is_pinned(&files.journal.conn, &first.parent.capture.observation_id).unwrap(),
        "releasing a queued owner preserves the baseline's independent pending-input owner"
    );
}

#[test]
fn releasing_a_queued_parent_never_clears_unconfirmed_native_focus_pins_or_source_debt() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut files = Files::create(&path, OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![page_activity()], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let question = focus(&input);
    let key = digest(b"queued-stage");
    files.pin_investigation_parent(&key, &question).unwrap();
    let reservation = files.reserve("fixture", &request(question.clone())).unwrap();
    let id = reservation.entry.identity.observation_id.clone();
    let native_owner = format!("focus:{id}");
    files.journal.source_work(&reservation.entry, SourceWorkState::Active).unwrap();
    drop(files);
    let mut files = Files::open(&path).unwrap();
    assert_eq!(files.recover().unwrap().source_debts, 1);
    files.release_investigation_parent(&key).unwrap();
    assert!(pin(&files, &native_owner));
    assert_eq!(files.journal.debts().unwrap(), 1);
    assert!(files.reserve("fixture", &request(question)).is_err());
    files.acknowledge_source_quiescence(&id).unwrap();
    assert!(!pin(&files, &native_owner));
    assert_eq!(files.journal.debts().unwrap(), 0);
}

#[test]
fn queued_parent_inventory_is_complete_and_capacity_never_drops_an_existing_owner() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![page_activity()], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let transaction = files.journal.conn.transaction().unwrap();
    for index in 0..MAX_DIAGNOSTIC_EPISODES {
        let key = digest(format!("queued-intent:{index}").as_bytes());
        transaction
            .execute(
                "INSERT INTO diagnostic_pins VALUES(?1,?2)",
                params![format!("investigation:{key}"), input.parent.capture.observation_id],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    let retained = files.investigation_parent_keys().unwrap();
    assert_eq!(retained.len(), MAX_DIAGNOSTIC_EPISODES);
    let overflow = digest(b"one-extra-intent");
    assert!(files.pin_investigation_parent(&overflow, &focus(&input)).is_err());
    assert_eq!(files.investigation_parent_keys().unwrap(), retained);
    files.release_investigation_parent(&retained[0]).unwrap();
    files.pin_investigation_parent(&overflow, &focus(&input)).unwrap();
    let owners = files.investigation_parent_keys().unwrap();
    assert_eq!(owners.len(), MAX_DIAGNOSTIC_EPISODES);
    assert!(owners.contains(&overflow));
}
#[test]
fn focus_reservation_authenticates_parent_before_creating_a_native_attempt_and_releases_never_started_work() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut files = Files::create(&path, OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![page_activity()], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let mut forged = focus(&input);
    forged.original.elapsed_ms = Some("90001".into());
    assert!(files.reserve("fixture", &request(forged)).is_err());
    assert_eq!(files.journal.status(10, None).unwrap().captures, 1);
    let reservation = files.reserve("fixture", &request(focus(&input))).unwrap();
    let owner = format!("focus:{}", reservation.entry.identity.observation_id);
    assert!(pin(&files, &owner));
    drop(files);
    let mut files = Files::open(&path).unwrap();
    let report = files.recover().unwrap();
    assert_eq!(report.interrupted, 1);
    assert_eq!(report.source_debts, 0);
    assert!(!pin(&files, &owner));
}
#[test]
fn unconfirmed_focused_work_keeps_its_parent_pin_until_explicit_quiescence_acknowledgement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut files = Files::create(&path, OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![page_activity()], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let reservation = files.reserve("fixture", &request(focus(&input))).unwrap();
    let id = reservation.entry.identity.observation_id.clone();
    let owner = format!("focus:{id}");
    files.journal.source_work(&reservation.entry, SourceWorkState::Active).unwrap();
    drop(files);
    let mut files = Files::open(&path).unwrap();
    assert_eq!(files.recover().unwrap().source_debts, 1);
    assert!(pin(&files, &owner));
    files.acknowledge_source_quiescence(&id).unwrap();
    assert!(!pin(&files, &owner));
    assert_eq!(files.journal.debts().unwrap(), 0);
}
#[test]
fn confirmed_cancelled_or_incomplete_focus_releases_its_pin_without_authorizing_a_successful_result() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![page_activity()], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let reservation = files.reserve("fixture", &request(focus(&input))).unwrap();
    let id = reservation.entry.identity.observation_id.clone();
    let owner = format!("focus:{id}");
    files.journal.source_work(&reservation.entry, SourceWorkState::Active).unwrap();
    files.journal.source_work(&reservation.entry, SourceWorkState::Quiescent).unwrap();
    files.finish(reservation).unwrap();
    assert!(!pin(&files, &owner));
    assert_eq!(files.journal.capture(&id).unwrap().state, OutboxCaptureState::Incomplete);
    assert_eq!(files.journal.debts().unwrap(), 0);
}

fn attach_page_report(input: &DiagnosticInput, episode_id: &str, focus: &ActivityWaitFocus) -> DiagnosticUpdate {
    let previous = input.checkpoint.as_ref().unwrap();
    let before = previous.episodes.iter().find(|episode| episode.id == episode_id).unwrap();
    let mut episode = before.clone();
    episode.analysis = DiagnosticAnalysisStatus::Partial;
    episode.issue = Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
    episode.report = Some(DiagnosticReport::PageWait { observation: input.rows[0].reference.clone() });
    let analyzed_at = "2026-11-14T08:03:00Z";
    let mut revision = DiagnosticRevision::between(Some(before), episode, analyzed_at).unwrap().unwrap();
    if !revision.inputs.contains(&focus.parent) {
        revision.inputs.push(focus.parent.clone());
    }
    revision.id = revision.expected_id().unwrap();
    let mut checkpoint = previous.clone();
    checkpoint.input = input.parent.clone();
    *checkpoint.episodes.iter_mut().find(|episode| episode.id == episode_id).unwrap() = revision.episode.clone();
    DiagnosticUpdate {
        step_id: digest(episode_id.as_bytes()),
        analyzed_at: analyzed_at.into(),
        revisions: vec![revision],
        checkpoint,
    }
}

#[test]
fn a_valid_page_report_cannot_be_attached_to_another_native_actor_in_the_same_database() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    let first = page_activity();
    let mut second = first.clone();
    second.key.session.id = "43".into();
    baseline(&mut files, vec![first, second], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let question = focus_at(&input, 0);
    let first_episode = page_episode(&input, &question);
    let second_episode = page_episode(&input, &focus_at(&input, 1));
    files
        .commit_diagnostics(
            input.clone(),
            update(&input, vec![first_episode.clone(), second_episode.clone()], vec![None, None], "two-waiters"),
        )
        .unwrap();
    let page = PageWaitObservation {
        source: "page wait targets".into(),
        observed_from: "2026-11-14T08:00:10Z".into(),
        observed_until: "2026-11-14T08:00:11Z".into(),
        request: question.original.clone(),
        before_check: None,
        after_check: None,
        statement_identity: None,
        plan_identity: None,
        resource: question.resource.clone(),
        observed_address: None,
        resolution: PageWaitResolution::Unavailable,
        target: None,
        index_operations: None,
    };
    let result = ActivityWaitFocusResult {
        focus: question.clone(),
        observed_from: page.observed_from.clone(),
        observed_until: page.observed_until.clone(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::SameStatement,
        wait: PageWaitRelation::OriginalObserved,
    };
    assert!(result.accepts_observation(&page));
    let source = CollectionDescription {
        target: input.source.target.clone(),
        source: "focused-page-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedPageWait {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitTargets,
                observed_from: result.observed_from.clone(),
                observed_until: result.observed_until.clone(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(page.source.clone(), 1, false)],
            },
            result: Box::new(result),
        },
    };
    let requested = request(question.clone());
    let reservation = files.reserve("fixture", &requested).unwrap();
    let capture = reservation.entry.identity.clone();
    files.journal.source_work(&reservation.entry, SourceWorkState::Active).unwrap();
    let mut writer = ObservationFiles::create(&reservation.directory, requested, capture.clone(), None).unwrap();
    writer.describe(source).unwrap();
    writer.begin_page(0).unwrap();
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: page.source.clone(),
        key: page.key().unwrap(),
        fact: OperationalFact::PageWait(Box::new(page)),
    });
    assert!(writer.write_page(0, &[record]).unwrap().is_none());
    writer.seal(0).unwrap();
    writer.confirm_source(SourceCompletion::Verified).unwrap();
    assert_eq!(writer.finish(1).unwrap().status, CollectionStatus::Complete);
    files.journal.source_work(&reservation.entry, SourceWorkState::Quiescent).unwrap();
    files.finish(reservation).unwrap();
    let current = files.next_diagnostic_input(1).unwrap().unwrap();
    assert_eq!(current.parent.capture, capture);
    let captures_before = files.status(10, None).unwrap().captures;
    let forged = attach_page_report(&current, &second_episode.id, &question);
    assert!(forged.revisions[0].is_valid(), "the receipt and proposed revision are independently valid");
    assert!(files.commit_diagnostics(current.clone(), forged).is_err());
    assert_eq!(files.status(10, None).unwrap().captures, captures_before);
    assert_eq!(files.next_diagnostic_input(1).unwrap().unwrap().parent, current.parent);
    let valid = attach_page_report(&current, &first_episode.id, &question);
    files.commit_diagnostics(current, valid).unwrap();
    let checkpoint = files.diagnostic_checkpoints(1, None, 10).unwrap().checkpoints.remove(0);
    let first = checkpoint.episodes.iter().find(|episode| episode.id == first_episode.id).unwrap();
    let second = checkpoint.episodes.iter().find(|episode| episode.id == second_episode.id).unwrap();
    assert!(first.report.is_some());
    assert_eq!(second, &second_episode, "rejecting the cross-report must leave the other actor untouched");
    assert!(files.next_diagnostic_input(1).unwrap().is_none());
}

#[tokio::test]
async fn parent_pin_survives_checkpoint_eviction_race_and_transfers_until_result_recovery_commit() {
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
    let mut files = Files::create_remote(&root.path().join("outbox"), OutboxLimits::default(), store.id()).unwrap();
    files.recover().unwrap();
    files.begin_remote_epoch(owner.fence(), 0).unwrap();
    let native = baseline(&mut files, vec![page_activity()], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let question = focus(&input);
    let initial = files
        .commit_diagnostics(
            input.clone(),
            update(&input, vec![page_episode(&input, &question)], vec![None], "initial-page-wait"),
        )
        .unwrap();
    let prepared = files.prepare_delivery(&native.observation_id, contract.clone(), vec![]).unwrap();
    let native_commit = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    files.mark_archived(&native_commit).unwrap();
    let prepared = files.prepare_delivery(&initial.observation_id, contract.clone(), vec![]).unwrap();
    let initial_commit =
        stage_and_commit(&store, owner.fence(), Some(&native_commit.manifest().range_id), &prepared).await.unwrap();
    let requested = request(question.clone());
    let queued_key = digest(b"queued-parent-handoff");
    files.pin_investigation_parent(&queued_key, &question).unwrap();
    let reservation = files.reserve("fixture", &requested).unwrap();
    let focus_owner = format!("focus:{}", reservation.entry.identity.observation_id);
    files.release_investigation_parent(&queued_key).unwrap();
    assert!(pin(&files, &focus_owner), "native reservation owns its parent before queued ownership is released");
    files.journal.source_work(&reservation.entry, SourceWorkState::Active).unwrap();
    files.mark_archived(&initial_commit).unwrap();
    assert!(!files.evict(&native_commit).unwrap(), "archiving the old head cannot evict an in-flight focus parent");
    let result = ActivityWaitFocusResult {
        focus: question,
        observed_from: "2026-11-14T08:00:10Z".into(),
        observed_until: "2026-11-14T08:00:11Z".into(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::Missing,
        wait: PageWaitRelation::Unqualified,
    };
    let source = CollectionDescription {
        target: input.source.target.clone(),
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
    let capture = reservation.entry.identity.clone();
    let mut writer = ObservationFiles::create(&reservation.directory, requested, capture.clone(), None).unwrap();
    writer.describe(source).unwrap();
    writer.seal(0).unwrap();
    writer.confirm_source(SourceCompletion::Verified).unwrap();
    assert_eq!(writer.finish(0).unwrap().status, CollectionStatus::Complete);
    files.journal.source_work(&reservation.entry, SourceWorkState::Quiescent).unwrap();
    files.finish(reservation).unwrap();
    assert!(pin(&files, &focus_owner));
    let current = files.next_diagnostic_input(1).unwrap().unwrap();
    let mut episode = current.checkpoint.as_ref().unwrap().episodes[0].clone();
    episode.revision = "2".into();
    episode.lifecycle = DiagnosticLifecycle::Resolved;
    episode.analysis = DiagnosticAnalysisStatus::Unresolved;
    episode.issue = Some(DiagnosticAnalysisIssue::NativeContextGone);
    episode.recovery = Some(DiagnosticRecoveryProof::FocusedParticipantGone {
        capture: current.parent.clone(),
        parent: Box::new(input.rows[0].reference.clone()),
    });
    let final_capture = files
        .commit_diagnostics(current.clone(), update(&current, vec![episode], vec![Some("1".into())], "focused-gone"))
        .unwrap();
    assert!(!pin(&files, &focus_owner));
    assert!(pin(&files, &format!("derived:{}", final_capture.observation_id)));
    assert!(!files.evict(&native_commit).unwrap());
    let prepared = files.prepare_delivery(&capture.observation_id, contract.clone(), vec![]).unwrap();
    let focus_commit =
        stage_and_commit(&store, owner.fence(), Some(&initial_commit.manifest().range_id), &prepared).await.unwrap();
    let prepared = files.prepare_delivery(&final_capture.observation_id, contract, vec![]).unwrap();
    let final_commit =
        stage_and_commit(&store, owner.fence(), Some(&focus_commit.manifest().range_id), &prepared).await.unwrap();
    files.mark_archived(&final_commit).unwrap();
    assert!(files.evict(&native_commit).unwrap());
    assert_eq!(files.journal.debts().unwrap(), 0);
}
