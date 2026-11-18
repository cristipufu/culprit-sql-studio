use super::shared::seal;
use super::*;
use crate::analytical::test_support::{PageWriterCase, investigation_fixture, page_writer_fixture};

pub(super) struct WriterInvestigation {
    pub root: tempfile::TempDir,
    pub files: Files,
    pub original: CaptureIdentity,
    pub page: CaptureIdentity,
    pub focus: PageWaitWriterFocus,
    pub intent: DiagnosticInvestigationIntent,
    source: CollectionDescription,
    records: Vec<TelemetryRecord>,
}

impl WriterInvestigation {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
        Self::from_files(root, files)
    }

    pub(super) fn from_files(root: tempfile::TempDir, mut files: Files) -> Self {
        let fixture = page_writer_fixture(PageWriterCase::ReportedBlockingSession);
        let original = seal(
            &mut files,
            request(CollectionSelection::Activity),
            fixture.activity_source,
            fixture.activity_rows,
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
            "2020-01-01T12:00:00.100000000Z",
        )
        .unwrap();
        let original_focus = observed.focuses.iter().find_map(|(_, focus)| focus.page_wait()).unwrap().clone();
        files
            .commit_diagnostics(
                input.clone(),
                DiagnosticUpdate {
                    step_id: digest(b"writer original native frame"),
                    analyzed_at: "2020-01-01T12:00:00.100000000Z".into(),
                    checkpoint: observed.checkpoint,
                    revisions: observed.revisions,
                },
            )
            .unwrap();
        let mut page_source = fixture.page_source;
        let mut snapshot = page_source.observation.snapshot().unwrap().clone();
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page_row), .. }) =
            &fixture.page_rows[0]
        else {
            unreachable!()
        };
        snapshot.observed_from = page_row.observed_from.clone();
        snapshot.observed_until = page_row.observed_until.clone();
        page_source.observation = SourceObservation::FocusedPageWait {
            snapshot,
            result: Box::new(ActivityWaitFocusResult {
                focus: original_focus.clone(),
                observed_from: page_row.observed_from.clone(),
                observed_until: page_row.observed_until.clone(),
                source: FocusSourceState::Matched,
                request: PageWaitRequestState::SameStatement,
                wait: PageWaitRelation::OriginalObserved,
            }),
        };
        let page_request = request(CollectionSelection::FocusedPageWait { focus: Box::new(original_focus.clone()) });
        assert!(
            page_source.matches_request(&page_request),
            "the focused native result and header retain the same read bounds"
        );
        let page = seal(&mut files, page_request, page_source, fixture.page_rows, None);
        let page_input = files.diagnostic_input(&page.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
        let mut focus = match &fixture.source.observation {
            SourceObservation::PageWaitWriters { focus, .. } => focus.as_ref().clone(),
            _ => unreachable!(),
        };
        focus.waiter = original_focus;
        focus.page = page_input.rows[0].reference.clone();
        for (candidate, row) in focus.candidates.iter_mut().zip(&input.rows[1..]) {
            candidate.parent = row.reference.clone();
        }
        let contexts = files.page_writer_contexts(&focus).unwrap();
        assert_eq!(contexts.len(), 2, "all original MARS contexts are retained, including candidate overlap");
        let page_rows = page_input
            .rows
            .iter()
            .map(|row| {
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::PageWait(observation),
                    ..
                }) = &row.row.record
                else {
                    unreachable!()
                };
                DiagnosticFocusRow { reference: &row.reference, observation }
            })
            .collect::<Vec<_>>();
        let mut explained = analyze_diagnostic_focus(
            &page_input.source,
            &page_input.parent,
            &page_rows,
            page_input.checkpoint.as_ref().unwrap(),
            "2020-01-01T12:00:01Z",
        )
        .unwrap();
        for revision in &mut explained.revisions {
            let Some(DiagnosticReport::PageWait { blocking_contexts, .. }) = &mut revision.episode.report else {
                continue;
            };
            *blocking_contexts = contexts.clone();
            for context in &contexts {
                if !revision.inputs.contains(context) {
                    revision.inputs.push(context.clone());
                }
            }
            revision.id = revision.expected_id().unwrap();
            let episode =
                explained.checkpoint.episodes.iter_mut().find(|episode| episode.id == revision.episode.id).unwrap();
            *episode = revision.episode.clone();
        }
        files
            .commit_diagnostics(
                page_input,
                DiagnosticUpdate {
                    step_id: digest(b"writer original mapped page"),
                    analyzed_at: "2020-01-01T12:00:01Z".into(),
                    checkpoint: explained.checkpoint.clone(),
                    revisions: explained.revisions,
                },
            )
            .unwrap();
        let episode = explained
            .checkpoint
            .episodes
            .iter()
            .find(|episode| episode.matches_page_wait_writer_focus(&focus))
            .unwrap();
        let mut recipe = investigation_fixture().recipe;
        recipe.id = "fixture.page-wait-writers".into();
        recipe.selector = RecipeSelector::FocusedPageWaitWriters;
        let RecipeCostQualification::Measured(cost) = &mut recipe.cost else { unreachable!() };
        cost.recipe_id = recipe.id.clone();
        cost.selector = recipe.selector;
        cost.scope = RecipeCostScope::Activity(episode.scope.clone());
        let component = cost.component.clone();
        let intent = DiagnosticInvestigationIntent::new(
            episode,
            &recipe,
            DiagnosticFocus::PageWaitWriters { focus: Box::new(focus.clone()) },
            &component,
            &CollectionBudget::default(),
        )
        .unwrap();
        let mut source = fixture.source;
        let SourceObservation::PageWaitWriters { focus: declared, .. } = &mut source.observation else {
            unreachable!()
        };
        **declared = focus.clone();
        let mut records = fixture.records;
        rebind_writer(&focus, &mut records[0]);
        Self { root, files, original, page, focus, intent, source, records }
    }

    pub(super) fn native(&mut self) -> CaptureIdentity {
        let request = CollectionRequest {
            subject: self.intent.subject(),
            selection: self.intent.selection().unwrap(),
            budget: self.intent.budget.clone(),
        };
        seal(
            &mut self.files,
            request,
            self.source.clone(),
            self.records.clone(),
            Some(DiagnosticInvestigationLineage::from_intent(&self.intent).unwrap()),
        )
    }

    fn proposal(&self, input: &DiagnosticInput) -> DiagnosticUpdate {
        let rows = input
            .rows
            .iter()
            .map(|row| {
                let TelemetryRecord::Operational(observation) = &row.row.record else { unreachable!() };
                DiagnosticPageWriterRow { reference: &row.reference, observation }
            })
            .collect::<Vec<_>>();
        let analyzed = analyze_diagnostic_page_writers(
            &input.source,
            &input.parent,
            &rows,
            input.checkpoint.as_ref().unwrap(),
            "2020-01-01T12:00:03Z",
            Some(&self.intent),
        )
        .unwrap();
        assert_eq!(analyzed.revisions.len(), 1);
        DiagnosticUpdate {
            step_id: digest(b"writer native relationship"),
            analyzed_at: "2020-01-01T12:00:03Z".into(),
            checkpoint: analyzed.checkpoint,
            revisions: analyzed.revisions,
        }
    }

    fn event(&mut self, event: DiagnosticInvestigationEvent, collecting: bool, at: &str) -> CaptureIdentity {
        let input = self.files.diagnostic_input(&self.page.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
        let mut checkpoint = input.checkpoint.clone().unwrap();
        let before = checkpoint.episodes.iter().find(|episode| episode.id == self.intent.episode_id).unwrap();
        let mut next = before.clone();
        next.analysis =
            if collecting { DiagnosticAnalysisStatus::Collecting } else { DiagnosticAnalysisStatus::Unresolved };
        next.issue = (!collecting).then_some(DiagnosticAnalysisIssue::EvidenceUnavailable);
        let revision = DiagnosticRevision::with_investigation(before, next, event, at).unwrap().unwrap();
        checkpoint.input = input.parent.clone();
        *checkpoint.episodes.iter_mut().find(|episode| episode.id == revision.episode.id).unwrap() =
            revision.episode.clone();
        self.files
            .commit_diagnostics(
                input,
                DiagnosticUpdate {
                    step_id: digest(revision.id.as_bytes()),
                    analyzed_at: at.into(),
                    checkpoint,
                    revisions: vec![revision],
                },
            )
            .unwrap()
    }
}

fn request(selection: CollectionSelection) -> CollectionRequest {
    CollectionRequest { subject: CollectionSubject::Database, selection, budget: CollectionBudget::default() }
}

fn rebind_writer(focus: &PageWaitWriterFocus, record: &mut TelemetryRecord) {
    let TelemetryRecord::Operational(row) = record else { unreachable!() };
    let OperationalFact::PageWaitWriters(point) = &mut row.fact else { unreachable!() };
    point.focus_key = focus.key().unwrap();
    for (candidate, original) in point.candidates.iter_mut().zip(&focus.candidates) {
        candidate.parent = original.parent.clone();
        if let Some(proof) = candidate.binding.take() {
            candidate.binding = Some(
                MssqlPageWritePlanProof::new(
                    &focus.target,
                    candidate,
                    proof.native_plan_sha256,
                    proof.native_statement_sha256,
                    proof.accesses,
                )
                .unwrap(),
            );
        }
    }
    row.key = point.key().unwrap();
}

#[test]
fn reservation_authenticates_the_original_page_and_entire_native_roster_before_allocating_work() {
    let mut fixture = WriterInvestigation::new();
    for field in ["page", "candidate", "statement", "generation", "scope", "role"] {
        let mut focus = fixture.focus.clone();
        match field {
            "page" => focus.page.payload_sha256 = digest(b"substituted original mapped page"),
            "candidate" => focus.candidates[0].parent.payload_sha256 = digest(b"substituted original candidate"),
            "statement" => focus.candidates[0].statement_identity.id = "another-original-statement".into(),
            "generation" => {
                focus.candidates[0].key.request.as_mut().unwrap().generation = "recycled-native-request".into();
                focus.candidates[0].parent.native_key = NativeRecordIdentity::Activity(focus.candidates[0].key.clone());
            }
            "scope" => {
                let original = &mut focus.waiter.participant;
                original.epoch = "another-native-process".into();
                let SourcePartition::InstanceLocal { epoch, .. } = &mut original.partition else { unreachable!() };
                *epoch = Some(original.epoch.clone());
            }
            "role" => focus.waiter.participant.database_writable = Some(false),
            _ => unreachable!(),
        }
        assert!(focus.is_valid(), "{field} must reach the authenticated original-input boundary");
        let before = fixture.files.journal.status(20, None).unwrap().captures;
        assert!(
            fixture
                .files
                .reserve("fixture", &request(CollectionSelection::PageWaitWriters { focus: Box::new(focus) }))
                .is_err(),
            "{field}"
        );
        assert_eq!(fixture.files.journal.status(20, None).unwrap().captures, before);
        assert_eq!(fixture.files.journal.debts().unwrap(), 0);
    }
    let native = fixture.native();
    let input = fixture.files.diagnostic_input(&native.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    assert_eq!(input.investigation_intent().unwrap().unwrap().key().unwrap(), fixture.intent.key().unwrap());
}

#[test]
fn durable_writer_reports_keep_page_candidates_and_all_historical_contexts_without_lifecycle_resolution() {
    let mut fixture = WriterInvestigation::new();
    fixture.files.pin_investigation_parent(&fixture.intent).unwrap();
    let key = fixture.intent.key().unwrap();
    let mut expected = vec![fixture.original.observation_id.clone(), fixture.page.observation_id.clone()];
    expected.sort();
    let mut queued = expected.clone();
    queued.push(fixture.intent.qualification.evidence.capture.observation_id.clone());
    queued.sort();
    assert_eq!(pins(&fixture.files, &format!("investigation:{key}")), queued);
    let native = fixture.native();
    assert_eq!(pins(&fixture.files, &format!("focus:{}", native.observation_id)), expected);
    let input = fixture.files.diagnostic_input(&native.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    let update = fixture.proposal(&input);
    let revision = &update.revisions[0];
    let report = revision.episode.report.as_ref().unwrap();
    let DiagnosticReport::PageWaitWriters { page, candidates, blocking_contexts, .. } = report else { unreachable!() };
    assert_eq!(page.as_ref(), &fixture.focus.page);
    assert_eq!(
        candidates,
        &fixture.focus.candidates.iter().map(|candidate| candidate.parent.clone()).collect::<Vec<_>>()
    );
    assert_eq!(blocking_contexts.len(), 2);
    assert_eq!(report.references().count(), 4, "candidate/context overlaps are role-aware and pinned once");
    assert!(report.references().all(|reference| revision.inputs.contains(reference)));
    assert_eq!(revision.episode.lifecycle, DiagnosticLifecycle::Active);
    assert!(revision.episode.recovery.is_none());
    let expected_report = report.clone();
    let derived = fixture.files.commit_diagnostics(input, update).unwrap();
    assert!(pins(&fixture.files, &format!("focus:{}", native.observation_id)).is_empty());
    let derived_pins = pins(&fixture.files, &format!("derived:{}", derived.observation_id));
    assert!(expected.iter().all(|capture| derived_pins.contains(capture)));
    fixture.files.release_investigation_parent(&key).unwrap();
    assert!(pins(&fixture.files, &format!("investigation:{key}")).is_empty());
    let path = fixture.root.path().join("outbox");
    drop(fixture.files);
    let mut files = Files::open(&path).unwrap();
    files.recover().unwrap();
    let current = files.diagnostic_input(&native.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    assert_eq!(
        current.checkpoint.as_ref().unwrap().episodes.iter().find_map(|episode| episode.report.as_ref()),
        Some(&expected_report)
    );
    assert_eq!(
        files.diagnostic_input_state(&current.parent, DIAGNOSTIC_CONTROLLER_VERSION).unwrap(),
        DiagnosticInputState::Consumed
    );
}

#[test]
fn omitted_or_substituted_writer_roles_cannot_commit_or_lose_original_contexts() {
    for field in ["page", "candidate", "context"] {
        let mut fixture = WriterInvestigation::new();
        let native = fixture.native();
        let input = fixture.files.diagnostic_input(&native.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
        let mut update = fixture.proposal(&input);
        let revision = &mut update.revisions[0];
        let Some(DiagnosticReport::PageWaitWriters { page, candidates, blocking_contexts, .. }) =
            &mut revision.episode.report
        else {
            unreachable!()
        };
        match field {
            "page" => page.payload_sha256 = digest(b"a different mapped native page"),
            "candidate" => {
                candidates.pop();
            }
            "context" => {
                blocking_contexts.pop();
            }
            _ => unreachable!(),
        }
        for reference in revision.episode.report.as_ref().unwrap().references() {
            if !revision.inputs.contains(reference) {
                revision.inputs.push(reference.clone());
            }
        }
        revision.id = revision.expected_id().unwrap();
        assert!(revision.is_valid(), "the native proof/roster guard must reject {field}");
        update.checkpoint.episodes = vec![revision.episode.clone()];
        let before = input.checkpoint.clone();
        assert!(fixture.files.commit_diagnostics(input.clone(), update).is_err(), "{field}");
        assert_eq!(
            fixture.files.diagnostic_input(&native.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap().checkpoint,
            before
        );
        assert_eq!(fixture.files.journal.debts().unwrap(), 0);
    }
}

#[test]
fn missing_original_frame_or_page_cannot_allocate_a_writer_capture_or_new_source_debt() {
    for missing_page in [false, true] {
        let mut fixture = WriterInvestigation::new();
        let missing = if missing_page { &fixture.page } else { &fixture.original };
        std::fs::remove_dir_all(fixture.files.directory(&missing.observation_id)).unwrap();
        let before = fixture.files.journal.status(20, None).unwrap().captures;
        assert!(fixture.files.reserve("fixture", &request(fixture.intent.selection().unwrap())).is_err());
        assert_eq!(fixture.files.journal.status(20, None).unwrap().captures, before);
        assert_eq!(fixture.files.journal.debts().unwrap(), 0);
        assert!(fixture.files.investigation_parent_keys().unwrap().is_empty());
    }
}

#[test]
fn failed_pre_authentication_writer_attempt_retains_full_question_and_both_parent_pins_after_restart() {
    let mut fixture = WriterInvestigation::new();
    fixture.files.pin_investigation_parent(&fixture.intent).unwrap();
    let admitted = DiagnosticInvestigationEvent {
        intent: Box::new(fixture.intent.clone()),
        attempt: None,
        source_work: SourceWorkState::NotStarted,
        outcome: DiagnosticInvestigationOutcome::Admitted,
    };
    fixture.event(admitted.clone(), true, "2020-01-01T12:00:01.100000000Z");
    let request = CollectionRequest {
        subject: fixture.intent.subject(),
        selection: fixture.intent.selection().unwrap(),
        budget: fixture.intent.budget.clone(),
    };
    let owner = DiagnosticInvestigationLineage::from_intent(&fixture.intent).unwrap();
    let reservation = fixture.files.reserve("fixture", &request).unwrap();
    let capture = reservation.entry.identity.clone();
    let writer =
        ObservationFiles::create(&reservation.directory, request, capture.clone(), Some(owner.clone())).unwrap();
    assert!(writer.manifest.description.is_none());
    assert!(matches!(writer.finish(0).unwrap().status, CollectionStatus::Incomplete(_)));
    fixture.files.finish(reservation).unwrap();
    let receipt = fixture.files.attempt_receipt(&capture, Some(&owner)).unwrap();
    assert!(receipt.manifest_sha256.is_some());
    let stopped = DiagnosticInvestigationEvent {
        attempt: Some(receipt.clone()),
        outcome: DiagnosticInvestigationOutcome::Stopped { issue: DiagnosticAnalysisIssue::EvidenceUnavailable },
        ..admitted
    };
    let history = fixture.event(stopped, false, "2020-01-01T12:00:01.200000000Z");
    let history_pins = pins(&fixture.files, &format!("derived:{}", history.observation_id));
    assert!(history_pins.contains(&fixture.original.observation_id));
    assert!(history_pins.contains(&fixture.page.observation_id));
    let key = fixture.intent.key().unwrap();
    let path = fixture.root.path().join("outbox");
    drop(fixture.files);
    let mut files = Files::open(&path).unwrap();
    files.recover().unwrap();
    assert_eq!(files.attempt_receipt(&capture, Some(&owner)).unwrap(), receipt);
    assert_eq!(files.investigation_parent_keys().unwrap().as_slice(), std::slice::from_ref(&key));
    let current = files.diagnostic_input(&fixture.page.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    let episode = current
        .checkpoint
        .as_ref()
        .unwrap()
        .episodes
        .iter()
        .find(|episode| episode.id == fixture.intent.episode_id)
        .unwrap();
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert!(episode.recovery.is_none());
    assert_eq!(files.journal.debts().unwrap(), 0);
}

#[tokio::test]
async fn cold_recovery_caches_the_focused_page_and_original_roster_without_resurrecting_native_work() {
    use crate::{
        delivery::{
            contract::Contract,
            diagnostic_recovery::{load_diagnostic_checkpoint, load_diagnostic_input},
            file_store::FileRecoveryStore,
        },
        outbox::Outbox,
    };
    let root = tempfile::tempdir().unwrap();
    let contract =
        Contract::new("pipeline".into(), "shard".into(), "file:writer-archive".into(), digest(b"writer-native-schema"))
            .unwrap();
    let store = FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let mut files = Files::create_remote(&root.path().join("outbox"), OutboxLimits::default(), store.id()).unwrap();
    files.recover().unwrap();
    files.begin_remote_epoch(owner.fence(), 0).unwrap();
    let mut fixture = WriterInvestigation::from_files(root, files);
    let writer = fixture.native();
    let mut entries = fixture.files.journal.status(20, None).unwrap().entries;
    entries.sort_by_key(|entry| entry.capture.revision.as_ref().unwrap().sequence.parse::<u64>().unwrap());
    for entry in entries {
        let prepared = fixture.files.prepare_delivery(&entry.capture.observation_id, contract.clone(), vec![]).unwrap();
        let head = store.checkpoint_head().await.unwrap();
        stage_and_commit(&store, owner.fence(), head.as_deref(), &prepared).await.unwrap();
    }
    let mut cold =
        Outbox::create_remote(&fixture.root.path().join("cold"), OutboxLimits::default(), store.id()).unwrap();
    cold.recover().await.unwrap();
    let head = store.diagnostic_heads(None, 10).await.unwrap().heads.remove(0);
    cold.restore_diagnostics(load_diagnostic_checkpoint(&store, &head).await.unwrap()).await.unwrap();
    let manifest = store.capture_manifest(&writer).await.unwrap();
    cold.restore_diagnostic_input("fixture", load_diagnostic_input(&store, &manifest.range_id).await.unwrap(), &store)
        .await
        .unwrap();
    cold.begin_diagnostic_epoch(owner.fence(), writer.revision.as_ref().unwrap().sequence.parse().unwrap())
        .await
        .unwrap();
    for reference in fixture.intent.focus.references() {
        assert!(
            cold.diagnostic_evidence(reference).await.is_err(),
            "prerequisites must come from authenticated recovery"
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
    for reference in fixture.intent.focus.references() {
        assert!(cold.diagnostic_evidence(reference).await.is_ok());
        assert_eq!(
            cold.diagnostic_input_state(&reference.capture_ref(), DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
            DiagnosticInputState::EvidenceOnly
        );
    }
    assert!(cold.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    let status = cold.status(20, None).await.unwrap();
    assert_eq!((status.captures, status.source_debts, status.unfinished), (1, 0, 0));
    let analyzed = cold.diagnostic_input(&writer.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    let report =
        analyzed.checkpoint.as_ref().unwrap().episodes.iter().find_map(|episode| episode.report.as_ref()).unwrap();
    let DiagnosticReport::PageWaitWriters { candidates, blocking_contexts, .. } = report else {
        panic!("writer stage must remain attached to its original database episode")
    };
    assert_eq!((candidates.len(), blocking_contexts.len()), (2, 2));
}

fn pins(files: &Files, owner: &str) -> Vec<String> {
    let mut query = files
        .journal
        .conn
        .prepare("SELECT observation_id FROM diagnostic_pins WHERE owner=?1 ORDER BY observation_id")
        .unwrap();
    query.query_map([owner], |row| row.get(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
}
