use super::*;
use crate::analytical::test_support::shared_investigation_fixture;

const ANALYZED_AT: &str = "2020-01-01T12:00:01Z";

struct SharedInvestigation {
    root: tempfile::TempDir,
    files: Files,
    original: CaptureIdentity,
    owner: CaptureIdentity,
    intent: DiagnosticInvestigationIntent,
}

impl SharedInvestigation {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
        Self::from_files(root, files)
    }

    fn with_recovery(root: tempfile::TempDir, store: &dyn RecoveryStore, fence: &Fence) -> Self {
        let mut files = Files::create_remote(&root.path().join("outbox"), OutboxLimits::default(), store.id()).unwrap();
        files.recover().unwrap();
        files.begin_remote_epoch(fence, 0).unwrap();
        Self::from_files(root, files)
    }

    fn from_files(root: tempfile::TempDir, mut files: Files) -> Self {
        let fixture = shared_investigation_fixture();
        let original = seal(
            &mut files,
            CollectionRequest {
                subject: CollectionSubject::Database,
                selection: CollectionSelection::Activity,
                budget: Default::default(),
            },
            fixture.investigation.baseline_source.clone(),
            vec![fixture.investigation.baseline_record.record.clone()],
            None,
        );
        let input = files.diagnostic_input(&original.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
        let rows = input
            .rows
            .iter()
            .map(|row| {
                let TelemetryRecord::Activity(observation) = &row.row.record else { unreachable!() };
                DiagnosticActivityRow { reference: &row.reference, observation }
            })
            .collect::<Vec<_>>();
        let observed = observe_diagnostic_activity(
            &input.source,
            &input.parent,
            &rows,
            None,
            &ActivityPolicy::default(),
            ANALYZED_AT,
        )
        .unwrap();
        let episode = observed.checkpoint.episodes[0].clone();
        let focus = observed.focuses[0].1.clone();
        files
            .commit_diagnostics(
                input,
                DiagnosticUpdate {
                    step_id: digest(b"shared-durable-initial"),
                    analyzed_at: ANALYZED_AT.into(),
                    revisions: observed.revisions,
                    checkpoint: observed.checkpoint,
                },
            )
            .unwrap();
        let owner = seal(
            &mut files,
            CollectionRequest {
                subject: fixture.investigation.intent.subject(),
                selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
                budget: Default::default(),
            },
            fixture.owner_source,
            vec![fixture.owner_record.record],
            None,
        );
        let (reference, source, row) = files.diagnostic_owner(&owner).unwrap().unwrap();
        // This structural fixture authenticates sealed bytes. Its synthetic cost receipt grants no native rights.
        let intent = DiagnosticInvestigationIntent::new_shared(
            &episode,
            &fixture.investigation.recipe,
            focus,
            &fixture.investigation.intent.qualification.component,
            &CollectionBudget::default(),
            DiagnosticSharedOwner { source: &source, row: &row, reference: &reference },
        )
        .unwrap();
        Self { root, files, original, owner, intent }
    }

    fn focused(&mut self, owned: bool, missing: bool) -> CaptureIdentity {
        let request = CollectionRequest {
            subject: self.intent.subject(),
            selection: self.intent.selection(),
            budget: self.intent.budget.clone(),
        };
        let result = ActivityWaitFocusResult {
            focus: self.intent.focus.page_wait().unwrap().clone(),
            observed_from: "2020-01-01T12:00:02Z".into(),
            observed_until: "2020-01-01T12:00:03Z".into(),
            source: FocusSourceState::Matched,
            request: if missing { PageWaitRequestState::Missing } else { PageWaitRequestState::SameStatement },
            wait: if missing { PageWaitRelation::Unqualified } else { PageWaitRelation::OriginalObserved },
        };
        let rows = if missing {
            vec![]
        } else {
            let page = PageWaitObservation {
                task_wait: None,
                source: "fixture-focused-shared".into(),
                observed_from: result.observed_from.clone(),
                observed_until: result.observed_until.clone(),
                request: result.focus.participant().original.clone(),
                before_check: None,
                after_check: None,
                statement_identity: None,
                plan_identity: None,
                resource: result.focus.resource.clone(),
                observed_address: None,
                resolution: PageWaitResolution::Unavailable,
                target: None,
                index_operations: None,
            };
            vec![TelemetryRecord::Operational(OperationalObservation {
                source: page.source.clone(),
                key: page.key().unwrap(),
                fact: OperationalFact::PageWait(Box::new(page)),
            })]
        };
        let source = CollectionDescription {
            target: self.intent.qualification.scope.target(),
            partition: self.intent.qualification.scope.partition().clone(),
            source: "fixture-focused-shared".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::FocusedPageWait {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageWaitTargets,
                    observed_from: result.observed_from.clone(),
                    observed_until: result.observed_until.clone(),
                    records: rows.len() as u32,
                    sources: vec![StateSourceCoverage::observed(
                        "fixture-focused-shared".into(),
                        rows.len() as u32,
                        false,
                    )],
                },
                result: Box::new(result),
            },
        };
        seal(
            &mut self.files,
            request,
            source,
            rows,
            owned.then(|| DiagnosticInvestigationLineage::from_intent(&self.intent).unwrap()),
        )
    }

    fn captures(&self) -> u32 {
        self.files.journal.conn.query_row("SELECT count(*) FROM captures", [], |row| row.get(0)).unwrap()
    }
}

fn seal(
    files: &mut Files,
    request: CollectionRequest,
    source: CollectionDescription,
    rows: Vec<TelemetryRecord>,
    investigation: Option<DiagnosticInvestigationLineage>,
) -> CaptureIdentity {
    let reservation = files.reserve("fixture", &request).unwrap();
    let identity = reservation.entry.identity.clone();
    files.journal.source_work(&reservation.entry, SourceWorkState::Active).unwrap();
    let mut writer =
        ObservationFiles::create(&reservation.directory, request, identity.clone(), investigation).unwrap();
    writer.describe(source).unwrap();
    writer.begin_page(0).unwrap();
    assert!(writer.write_page(0, &rows).unwrap().is_none());
    writer.seal(0).unwrap();
    writer.confirm_source(SourceCompletion::Verified).unwrap();
    assert_eq!(writer.finish(1).unwrap().status, CollectionStatus::Complete);
    files.journal.source_work(&reservation.entry, SourceWorkState::Quiescent).unwrap();
    files.finish(reservation).unwrap();
    identity
}

#[test]
fn shared_input_keeps_immutable_lineage_and_the_original_database_head_after_restart() {
    let mut fixture = SharedInvestigation::new();
    let focused = fixture.focused(true, false);
    let input = fixture.files.diagnostic_input(&focused.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    assert_ne!(input.source.target, fixture.intent.focus.participant().target);
    assert_eq!(input.controller_target().unwrap(), &fixture.intent.focus.participant().target);
    assert_eq!(input.checkpoint.as_ref().unwrap().target, fixture.intent.focus.participant().target);
    assert_eq!(input.investigation_intent().unwrap().unwrap().key().unwrap(), fixture.intent.key().unwrap());
    let expected = input.checkpoint.clone();
    let path = fixture.root.path().join("outbox");
    drop(fixture.files);
    let mut reopened = Files::open(&path).unwrap();
    reopened.recover().unwrap();
    let input = reopened.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).unwrap().unwrap();
    assert_eq!(input.parent.capture, focused);
    assert_eq!(input.checkpoint, expected);
    assert_eq!(input.controller_target().unwrap(), &fixture.intent.focus.participant().target);
    assert_eq!(input.investigation_intent().unwrap().unwrap().key().unwrap(), fixture.intent.key().unwrap());
}

#[test]
fn one_investigation_pin_owner_protects_both_authenticated_parents_and_rejects_substituted_rows() {
    let mut fixture = SharedInvestigation::new();
    let mut wrong = fixture.intent.clone();
    let DiagnosticExecutionBinding::Shared { owner } = &mut wrong.execution else { unreachable!() };
    owner.row_ordinal = 1;
    assert!(wrong.is_valid());
    assert!(fixture.files.pin_investigation_parent(&wrong).is_err());
    assert!(fixture.files.investigation_parent_keys().unwrap().is_empty());
    fixture.files.pin_investigation_parent(&fixture.intent).unwrap();
    fixture.files.pin_investigation_parent(&fixture.intent).unwrap();
    let key = fixture.intent.key().unwrap();
    assert_eq!(fixture.files.investigation_parent_keys().unwrap().as_slice(), std::slice::from_ref(&key));
    let count: u32 = fixture
        .files
        .journal
        .conn
        .query_row("SELECT count(*) FROM diagnostic_pins WHERE owner=?1", [format!("investigation:{key}")], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 2);
    for capture in [&fixture.original, &fixture.owner] {
        assert!(super::super::journal::is_pinned(&fixture.files.journal.conn, &capture.observation_id).unwrap());
    }
    fixture.files.release_investigation_parent(&key).unwrap();
    assert!(fixture.files.investigation_parent_keys().unwrap().is_empty());
}

#[tokio::test]
async fn manual_shared_focus_never_enters_the_database_controller_and_its_parent_pin_retires_on_archive() {
    let root = tempfile::tempdir().unwrap();
    let contract = crate::delivery::contract::Contract::new(
        "pipeline".into(),
        "shard".into(),
        "file:warehouse".into(),
        digest(b"shared-durable-schema"),
    )
    .unwrap();
    let store = crate::delivery::file_store::FileRecoveryStore::create(&root.path().join("recovery"), contract.clone())
        .unwrap();
    let owner = store.acquire().await.unwrap();
    let mut fixture = SharedInvestigation::with_recovery(root, &store, owner.fence());
    let focused = fixture.focused(false, true);
    assert!(fixture.files.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).unwrap().is_none());
    assert!(fixture.files.diagnostic_input(&focused.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).is_err());
    let focus_owner = format!("focus:{}", focused.observation_id);
    let archive_owner = format!("derived:{}", focused.observation_id);
    assert!(!super::focus::pin(&fixture.files, &focus_owner));
    assert!(super::focus::pin(&fixture.files, &archive_owner));
    let prepared = fixture.files.prepare_delivery(&focused.observation_id, contract, vec![]).unwrap();
    let committed = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    assert!(store.pending_diagnostic_inputs(None, 10).await.unwrap().ranges.is_empty());
    fixture.files.mark_archived(&committed).unwrap();
    assert!(!super::focus::pin(&fixture.files, &archive_owner));
}

#[tokio::test]
async fn offline_shared_result_updates_only_the_original_database_checkpoint() {
    let mut fixture = SharedInvestigation::new();
    let focused = fixture.focused(true, false);
    let native_source =
        fixture.files.diagnostic_input(&focused.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap().source;
    let path = fixture.root.path().join("outbox");
    drop(fixture.files);
    let mut outbox = crate::outbox::Outbox::open(&path).unwrap();
    outbox.recover().await.unwrap();
    let policy = crate::diagnostics::DiagnosticPolicies::for_activity(
        "fixture",
        CollectionSubject::Database,
        ActivityPolicy::default(),
    )
    .unwrap();
    assert_eq!(
        crate::diagnostics::analyze_pending(
            &mut outbox,
            &policy,
            &crate::capture::CollectionCancellation::default(),
            1,
            None
        )
        .await
        .unwrap(),
        1
    );
    let input = outbox.diagnostic_input(&focused.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    assert_eq!(input.source, native_source);
    let checkpoint = input.checkpoint.unwrap();
    assert_eq!(checkpoint.target, fixture.intent.focus.participant().target);
    assert_eq!(checkpoint.episodes.len(), 1);
    assert_eq!(checkpoint.episodes[0].first, fixture.intent.focus.participant().parent);
    assert_eq!(checkpoint.episodes[0].report.as_ref().unwrap().evidence().capture, focused);
    assert_eq!(
        outbox.diagnostic_input_state(&input.parent, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Consumed
    );
}

#[test]
fn a_shared_intent_or_epoch_substitution_is_rejected_before_derived_allocation() {
    let mut fixture = SharedInvestigation::new();
    let focused = fixture.focused(true, true);
    let input = fixture.files.diagnostic_input(&focused.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    let previous = input.checkpoint.as_ref().unwrap();
    let result = analyze_diagnostic_focus_with_intent(
        &input.source,
        &input.parent,
        &[],
        previous,
        "2020-01-01T12:00:04Z",
        &fixture.intent,
    )
    .unwrap();
    let update = DiagnosticUpdate {
        step_id: digest(b"shared-missing-focus"),
        analyzed_at: "2020-01-01T12:00:04Z".into(),
        revisions: result.revisions,
        checkpoint: result.checkpoint,
    };
    let before = fixture.captures();
    let mut replaced = input.clone();
    let owner = replaced.investigation.as_mut().unwrap();
    owner.qualification.read_metrics.templates[0].client_elapsed_ms = -0.0;
    assert!(fixture.files.commit_diagnostics(replaced, update.clone()).is_err());
    assert_eq!(fixture.captures(), before);
    let mut foreign = input;
    let SourcePartition::InstanceLocal { epoch, .. } = &mut foreign.source.partition else { unreachable!() };
    *epoch = Some("a different shared native process".into());
    assert!(fixture.files.commit_diagnostics(foreign, update).is_err());
    assert_eq!(fixture.captures(), before);
}

#[test]
fn zero_or_unknown_shared_native_owners_do_not_become_positive_admission_references() {
    let mut fixture = SharedInvestigation::new();
    let model = shared_investigation_fixture();
    let request = CollectionRequest {
        subject: model.investigation.intent.subject(),
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
        budget: Default::default(),
    };
    let mut empty = model.owner_source.clone();
    let SourceObservation::Snapshot(snapshot) = &mut empty.observation else { unreachable!() };
    snapshot.records = 0;
    snapshot.sources[0] = StateSourceCoverage::collected(empty.source.clone(), 0);
    let capture = seal(&mut fixture.files, request.clone(), empty, vec![], None);
    assert!(fixture.files.diagnostic_owner(&capture).unwrap().is_none());

    let mut unknown = model.owner_source;
    unknown.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    let SourceObservation::Snapshot(snapshot) = &mut unknown.observation else { unreachable!() };
    snapshot.sources[0] = StateSourceCoverage::observed(unknown.source.clone(), 1, false);
    assert!(unknown.partition_is_consistent());
    let capture = seal(&mut fixture.files, request, unknown, vec![model.owner_record.record], None);
    assert!(fixture.files.diagnostic_owner(&capture).unwrap().is_none());
    assert!(fixture.files.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).unwrap().is_none());
    let mut foreign = fixture.owner.clone();
    foreign.revision.as_mut().unwrap().sequence = "999".into();
    assert!(fixture.files.diagnostic_owner(&foreign).is_err());
}

#[test]
fn a_shared_page_snapshot_is_retained_without_becoming_an_operational_owner_binding() {
    let mut fixture = SharedInvestigation::new();
    let (source, rows) = crate::analytical::test_support::shared_page_wait_evidence_cases().remove(0);
    assert!(!rows.is_empty());
    let CollectionTarget::Shared(resource) = &source.target else { unreachable!() };
    let request = CollectionRequest {
        subject: CollectionSubject::Shared { resource: resource.clone() },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets },
        budget: Default::default(),
    };
    let capture = seal(&mut fixture.files, request, source.clone(), rows, None);
    assert!(fixture.files.diagnostic_owner(&capture).unwrap().is_none());
    assert_eq!(fixture.files.verify(&capture.observation_id).unwrap().description, Some(source));
    assert!(fixture.files.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).unwrap().is_none());
}

#[test]
fn local_lineage_metadata_cannot_replace_the_sealed_native_intent() {
    let mut fixture = SharedInvestigation::new();
    let focused = fixture.focused(true, true);
    let mut foreign = fixture.intent.clone();
    foreign.qualification.read_metrics.templates[0].client_elapsed_ms += 1.0;
    let lineage = DiagnosticInvestigationLineage::from_intent(&foreign).unwrap();
    assert!(foreign.matches_result_source(
        &fixture.files.diagnostic_input(&focused.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap().source
    ));
    fixture
        .files
        .journal
        .conn
        .execute(
            "UPDATE diagnostic_inputs SET lineage_json=?1 WHERE observation_id=?2",
            params![encode(&lineage).unwrap(), focused.observation_id],
        )
        .unwrap();
    assert!(fixture.files.diagnostic_input(&focused.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).is_err());
}

#[tokio::test]
async fn cold_shared_inputs_authenticate_and_cache_both_originals_without_reviving_native_work() {
    let root = tempfile::tempdir().unwrap();
    let contract = crate::delivery::contract::Contract::new(
        "pipeline".into(),
        "shard".into(),
        "file:warehouse".into(),
        digest(b"cold-shared-schema"),
    )
    .unwrap();
    let store = crate::delivery::file_store::FileRecoveryStore::create(&root.path().join("recovery"), contract.clone())
        .unwrap();
    let ownership = store.acquire().await.unwrap();
    let mut fixture = SharedInvestigation::with_recovery(root, &store, ownership.fence());
    let focused = fixture.focused(true, false);
    let mut entries = fixture.files.journal.status(10, None).unwrap().entries;
    entries.sort_by_key(|entry| entry.capture.revision.as_ref().unwrap().sequence.parse::<u64>().unwrap());
    for entry in entries {
        let prepared = fixture.files.prepare_delivery(&entry.capture.observation_id, contract.clone(), vec![]).unwrap();
        let head = store.checkpoint_head().await.unwrap();
        stage_and_commit(&store, ownership.fence(), head.as_deref(), &prepared).await.unwrap();
    }
    let cold_path = fixture.root.path().join("cold");
    let mut cold = crate::outbox::Outbox::create_remote(&cold_path, OutboxLimits::default(), store.id()).unwrap();
    cold.recover().await.unwrap();
    let head = store.diagnostic_heads(None, 10).await.unwrap().heads.remove(0);
    cold.restore_diagnostics(
        crate::delivery::diagnostic_recovery::load_diagnostic_checkpoint(&store, &head).await.unwrap(),
    )
    .await
    .unwrap();
    let native = store.capture_manifest(&focused).await.unwrap();
    cold.restore_diagnostic_input(
        "fixture",
        crate::delivery::diagnostic_recovery::load_diagnostic_input(&store, &native.range_id).await.unwrap(),
        &store,
    )
    .await
    .unwrap();
    cold.begin_diagnostic_epoch(ownership.fence(), focused.revision.as_ref().unwrap().sequence.parse().unwrap())
        .await
        .unwrap();
    let pending = cold.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().unwrap();
    assert_eq!(pending.investigation_intent().unwrap().unwrap().key().unwrap(), fixture.intent.key().unwrap());
    let owner = match &fixture.intent.execution {
        DiagnosticExecutionBinding::Shared { owner } => owner.as_ref(),
        _ => unreachable!(),
    };
    for reference in [&fixture.intent.focus.participant().parent, owner] {
        assert_eq!(
            cold.diagnostic_evidence(reference).await.unwrap_err().detail,
            Some(culprit_core::ErrorDetail::Outbox(OutboxProblem::MissingCapture))
        );
    }
    let policies = crate::diagnostics::DiagnosticPolicies::for_activity(
        "fixture",
        CollectionSubject::Database,
        ActivityPolicy::default(),
    )
    .unwrap();
    assert_eq!(
        crate::diagnostics::analyze_pending(
            &mut cold,
            &policies,
            &crate::capture::CollectionCancellation::default(),
            1,
            Some(&store)
        )
        .await
        .unwrap(),
        1
    );
    for reference in [&fixture.intent.focus.participant().parent, owner] {
        assert!(cold.diagnostic_evidence(reference).await.is_ok());
        assert_eq!(
            cold.diagnostic_input_state(&reference.capture_ref(), DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
            DiagnosticInputState::EvidenceOnly
        );
    }
    assert!(cold.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    let status = cold.status(10, None).await.unwrap();
    assert_eq!((status.captures, status.source_debts, status.unfinished), (1, 0, 0));
    let derived = &status.entries[0].capture;
    let prepared = cold.delivery().prepare(&derived.observation_id, contract, vec![]).await.unwrap();
    let previous = store.checkpoint_head().await.unwrap();
    let owner_manifest = store.capture_manifest(&owner.capture).await.unwrap();
    let owner_object = fixture.root.path().join("recovery").join(&owner_manifest.batches[0].object);
    let owner_bytes = std::fs::read(&owner_object).unwrap();
    std::fs::write(&owner_object, &owner_bytes[..owner_bytes.len() - 1]).unwrap();
    assert!(
        stage_and_commit(&store, ownership.fence(), previous.as_deref(), &prepared).await.is_err(),
        "archive ownership independently authenticates the positive shared owner through EOF"
    );
    assert_eq!(store.checkpoint_head().await.unwrap(), previous);
    assert!(store.diagnostic_input_pending(&pending.parent, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap());
    std::fs::write(&owner_object, owner_bytes).unwrap();
    let committed = stage_and_commit(&store, ownership.fence(), previous.as_deref(), &prepared).await.unwrap();
    let head = store.diagnostic_heads(None, 10).await.unwrap().heads.remove(0);
    let restored = crate::delivery::diagnostic_recovery::load_diagnostic_checkpoint(&store, &head).await.unwrap();
    assert_eq!(restored.checkpoint().target, fixture.intent.focus.participant().target);
    assert_eq!(restored.checkpoint().input.capture, focused);
    assert_eq!(head.range_id, committed.manifest().range_id);
    assert!(!store.diagnostic_input_pending(&pending.parent, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap());
    drop(cold);
    let mut reopened = crate::outbox::Outbox::open(&cold_path).unwrap();
    reopened.recover().await.unwrap();
    assert!(reopened.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    assert_eq!(
        reopened
            .diagnostic_input(&focused.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
            .await
            .unwrap()
            .controller_target()
            .unwrap(),
        &fixture.intent.focus.participant().target
    );
}
