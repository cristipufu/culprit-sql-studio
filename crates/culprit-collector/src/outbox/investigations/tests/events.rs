use super::focus::{admitted_intent, focus, page_activity, page_episode};
use super::*;

const ANALYZED_AT: &str = "2026-11-14T08:03:00Z";

struct Harness {
    root: tempfile::TempDir,
    files: Files,
    original: CaptureIdentity,
    intent: DiagnosticInvestigationIntent,
}
impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
        let original = baseline(&mut files, vec![page_activity()], true, 0);
        let input = files.next_diagnostic_input(1).unwrap().unwrap();
        let episode = page_episode(&input, &focus(&input));
        files.commit_diagnostics(input.clone(), update(&input, vec![episode], vec![None], "page-baseline")).unwrap();
        let input = files.diagnostic_input(&original.observation_id, 1).unwrap();
        let intent = admitted_intent(&input);
        Self { root, files, original, intent }
    }

    fn input(&mut self) -> DiagnosticInput {
        self.files.diagnostic_input(&self.original.observation_id, 1).unwrap()
    }

    fn episode(&mut self) -> DiagnosticEpisode {
        self.input().checkpoint.unwrap().episodes.remove(0)
    }

    fn admitted(&self) -> DiagnosticInvestigationEvent {
        // The approval is a structural synthetic fixture. This test never approves or opens a native source.
        DiagnosticInvestigationEvent {
            intent: Box::new(self.intent.clone()),
            attempt: None,
            source_work: SourceWorkState::NotStarted,
            outcome: DiagnosticInvestigationOutcome::Admitted,
        }
    }

    fn failed_before_authentication(&mut self) -> DiagnosticInvestigationEvent {
        let request = CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::FocusedPageWait { focus: Box::new(self.intent.focus.clone()) },
            budget: self.intent.budget.clone(),
        };
        let owner = DiagnosticInvestigationLineage::from_intent(&self.intent).unwrap();
        let reservation = self.files.reserve("fixture", &request).unwrap();
        let identity = reservation.entry.identity.clone();
        let writer =
            ObservationFiles::create(&reservation.directory, request, identity.clone(), Some(owner.clone())).unwrap();
        assert!(writer.manifest.description.is_none());
        assert!(matches!(writer.finish(0).unwrap().status, CollectionStatus::Incomplete(_)));
        self.files.finish(reservation).unwrap();
        let receipt = self.files.attempt_receipt(&identity, Some(&owner)).unwrap();
        assert!(receipt.manifest_sha256.is_some());
        assert_eq!(
            self.files.journal.capture(&identity.observation_id).unwrap().source_work,
            SourceWorkState::NotStarted
        );
        DiagnosticInvestigationEvent {
            attempt: Some(receipt),
            outcome: DiagnosticInvestigationOutcome::Stopped { issue: DiagnosticAnalysisIssue::EvidenceUnavailable },
            ..self.admitted()
        }
    }

    fn captured(&mut self) -> DiagnosticInvestigationEvent {
        let request = CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::FocusedPageWait { focus: Box::new(self.intent.focus.clone()) },
            budget: self.intent.budget.clone(),
        };
        let owner = DiagnosticInvestigationLineage::from_intent(&self.intent).unwrap();
        let reservation = self.files.reserve("fixture", &request).unwrap();
        let identity = reservation.entry.identity.clone();
        let source = "focused-native-pages".to_owned();
        let description = CollectionDescription {
            target: self.intent.focus.target.clone(),
            source: source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::FocusedPageWait {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageWaitTargets,
                    observed_from: "2026-11-14T08:00:02Z".into(),
                    observed_until: "2026-11-14T08:00:03Z".into(),
                    records: 0,
                    sources: vec![StateSourceCoverage::observed(source, 0, false)],
                },
                result: Box::new(ActivityWaitFocusResult {
                    focus: self.intent.focus.clone(),
                    observed_from: "2026-11-14T08:00:02Z".into(),
                    observed_until: "2026-11-14T08:00:03Z".into(),
                    source: FocusSourceState::Matched,
                    request: PageWaitRequestState::Missing,
                    wait: PageWaitRelation::Unqualified,
                }),
            },
        };
        let mut writer =
            ObservationFiles::create(&reservation.directory, request, identity.clone(), Some(owner.clone())).unwrap();
        self.files.journal.source_work(&reservation.entry, SourceWorkState::Active).unwrap();
        writer.describe(description).unwrap();
        writer.seal(0).unwrap();
        writer.confirm_source(SourceCompletion::Verified).unwrap();
        assert_eq!(writer.finish(1).unwrap().status, CollectionStatus::Complete);
        self.files.journal.source_work(&reservation.entry, SourceWorkState::Quiescent).unwrap();
        self.files.finish(reservation).unwrap();
        let input = self.files.diagnostic_input(&identity.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
        DiagnosticInvestigationEvent {
            attempt: Some(self.files.attempt_receipt(&identity, Some(&owner)).unwrap()),
            source_work: SourceWorkState::Quiescent,
            outcome: DiagnosticInvestigationOutcome::Captured { capture: input.parent },
            ..self.admitted()
        }
    }

    fn proposal(&mut self, revision: DiagnosticRevision) -> (DiagnosticInput, DiagnosticUpdate) {
        let input = self.input();
        let mut checkpoint = input.checkpoint.clone().unwrap();
        checkpoint.input = input.parent.clone();
        checkpoint.episodes = vec![revision.episode.clone()];
        let proposed = DiagnosticUpdate {
            step_id: digest(format!("event-test:{}", revision.id).as_bytes()),
            analyzed_at: revision.analyzed_at.clone(),
            revisions: vec![revision],
            checkpoint,
        };
        (input, proposed)
    }

    fn commit(&mut self, revision: DiagnosticRevision) -> CaptureIdentity {
        let (input, proposed) = self.proposal(revision);
        self.files.commit_diagnostics(input, proposed).unwrap()
    }

    fn capture_count(&self) -> u32 {
        self.files.journal.conn.query_row("SELECT count(*) FROM captures", [], |row| row.get(0)).unwrap()
    }

    fn reject(&mut self, revision: DiagnosticRevision) {
        self.reject_with_problem(revision, OutboxProblem::Integrity);
    }

    fn reject_with_problem(&mut self, revision: DiagnosticRevision, problem: OutboxProblem) {
        assert!(revision.is_valid(), "the test must reach the actual stored-state CAS guard");
        let before = self.episode();
        let count = self.capture_count();
        let (input, proposed) = self.proposal(revision);
        let error = self.files.commit_diagnostics(input, proposed).unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Outbox(problem)));
        assert_eq!(self.episode(), before);
        assert_eq!(self.capture_count(), count, "refusal happens before reserving or writing derived history");
        assert_eq!(self.files.journal.debts().unwrap(), 0);
    }
}

fn raw_revision(
    before: &DiagnosticEpisode,
    mut next: DiagnosticEpisode,
    investigation: Option<DiagnosticInvestigationEvent>,
) -> DiagnosticRevision {
    next.revision = (before.revision.parse::<u64>().unwrap() + 1).to_string();
    let mut inputs = vec![next.latest.clone()];
    if let Some(event) = &investigation
        && !inputs.contains(&event.intent.focus.parent)
    {
        inputs.push(event.intent.focus.parent.clone());
    }
    let mut revision = DiagnosticRevision {
        id: String::new(),
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        episode: next,
        previous_revision: Some(before.revision.clone()),
        inputs,
        analyzed_at: ANALYZED_AT.into(),
        investigation: investigation.map(Box::new),
    };
    revision.id = revision.expected_id().unwrap();
    revision
}

#[test]
fn compact_pointer_changes_require_new_full_provenance_and_cannot_clear_earlier_history() {
    let mut harness = Harness::new();
    let before = harness.episode();
    let mut forged = before.clone();
    forged.investigation_event = Some(digest(b"unattached-owner-event"));
    harness.reject(raw_revision(&before, forged, None));

    let revision = DiagnosticRevision::with_investigation(&before, before.clone(), harness.admitted(), ANALYZED_AT)
        .unwrap()
        .unwrap();
    harness.commit(revision);
    let before = harness.episode();
    for pointer in [None, Some(digest(b"a-second-unattached-event"))] {
        let mut forged = before.clone();
        forged.investigation_event = pointer;
        harness.reject(raw_revision(&before, forged, None));
    }
}

#[test]
fn an_unchanged_event_is_not_repeated_in_a_different_state_transition() {
    let mut harness = Harness::new();
    let before = harness.episode();
    let event = harness.admitted();
    let revision =
        DiagnosticRevision::with_investigation(&before, before.clone(), event.clone(), ANALYZED_AT).unwrap().unwrap();
    harness.commit(revision);
    let before = harness.episode();
    let mut next = before.clone();
    next.analysis = DiagnosticAnalysisStatus::Deferred;
    next.issue = Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
    harness.reject(raw_revision(&before, next.clone(), Some(event)));
    let native = DiagnosticRevision::between(Some(&before), next, ANALYZED_AT).unwrap().unwrap();
    assert!(native.investigation.is_none());
    let expected = native.episode.clone();
    harness.commit(native);
    assert_eq!(harness.episode(), expected);
}

#[test]
fn events_must_refer_to_an_existing_episode_revision_not_the_revision_they_are_creating() {
    let mut harness = Harness::new();
    let before = harness.episode();
    let mut event = harness.admitted();
    event.intent.episode_revision = (before.revision.parse::<u64>().unwrap() + 1).to_string();
    let mut next = before.clone();
    next.investigation_event = Some(event.key().unwrap());
    harness.reject(raw_revision(&before, next, Some(event)));

    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    baseline(&mut files, vec![page_activity()], true, 0);
    let input = files.next_diagnostic_input(1).unwrap().unwrap();
    let intent = admitted_intent(&input);
    let event = DiagnosticInvestigationEvent {
        intent: Box::new(intent),
        attempt: None,
        source_work: SourceWorkState::NotStarted,
        outcome: DiagnosticInvestigationOutcome::Admitted,
    };
    let mut native = update(&input, vec![page_episode(&input, &focus(&input))], vec![None], "forged-newborn-admission");
    native.revisions[0].episode.investigation_event = Some(event.key().unwrap());
    native.revisions[0].investigation = Some(Box::new(event));
    native.revisions[0].id = native.revisions[0].expected_id().unwrap();
    native.checkpoint.episodes[0] = native.revisions[0].episode.clone();
    assert!(native.revisions[0].is_valid());
    assert!(files.commit_diagnostics(input.clone(), native).is_err());
    assert_eq!(files.diagnostic_input_state(&input.parent, 1).unwrap(), DiagnosticInputState::Pending);
}

#[test]
fn full_event_original_is_authenticated_even_when_its_compact_native_relation_is_structurally_valid() {
    let mut harness = Harness::new();
    let before = harness.episode();
    for changed in ["original", "receipt"] {
        let mut event = harness.admitted();
        if changed == "original" {
            event.intent.focus.original.elapsed_ms = Some("90001".into());
        } else {
            event.intent.focus.parent.row_ordinal += 1;
        }
        assert!(event.matches_episode(&before));
        let revision =
            DiagnosticRevision::with_investigation(&before, before.clone(), event, ANALYZED_AT).unwrap().unwrap();
        harness.reject(revision);
    }
}

#[test]
fn sealed_event_replay_is_idempotent_but_a_distinct_failed_attempt_creates_same_status_history() {
    let mut harness = Harness::new();
    let before = harness.episode();
    let mut next = before.clone();
    next.analysis = DiagnosticAnalysisStatus::Deferred;
    next.issue = Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
    let first_event = harness.failed_before_authentication();
    let first =
        DiagnosticRevision::with_investigation(&before, next, first_event.clone(), ANALYZED_AT).unwrap().unwrap();
    let (input, proposed) = harness.proposal(first.clone());
    let captured = harness.files.commit_diagnostics(input.clone(), proposed.clone()).unwrap();
    let count = harness.capture_count();
    assert_eq!(harness.files.commit_diagnostics(input.clone(), proposed.clone()).unwrap(), captured);
    assert_eq!(harness.capture_count(), count);
    assert!(
        DiagnosticRevision::with_investigation(&first.episode, first.episode.clone(), first_event, ANALYZED_AT)
            .unwrap()
            .is_none()
    );
    let before = harness.episode();
    let second_event = harness.failed_before_authentication();
    let second = DiagnosticRevision::with_investigation(&before, before.clone(), second_event.clone(), ANALYZED_AT)
        .unwrap()
        .unwrap();
    assert_eq!(second.episode.analysis, first.episode.analysis);
    assert_eq!(second.episode.issue, first.episode.issue);
    assert_ne!(second.id, first.id);
    assert_ne!(second.episode.investigation_event, first.episode.investigation_event);
    assert_ne!(second_event.attempt, first.investigation.as_ref().unwrap().attempt);
    let expected = second.episode.clone();
    harness.commit(second);
    assert_eq!(harness.episode(), expected);
    assert_eq!(harness.files.journal.debts().unwrap(), 0);

    let path = harness.root.path().join("outbox");
    drop(harness.files);
    let mut reopened = Files::open(&path).unwrap();
    reopened.recover().unwrap();
    assert_eq!(
        reopened.commit_diagnostics(input, proposed).unwrap(),
        captured,
        "lost acknowledgement after restart adds no history"
    );
    assert_eq!(
        reopened.diagnostic_input(&harness.original.observation_id, 1).unwrap().checkpoint.unwrap().episodes[0],
        expected
    );
}

#[test]
fn stopped_events_reject_foreign_or_missing_attempts_and_forged_manifest_digests() {
    let mut harness = Harness::new();
    for changed in ["foreign", "missing", "missing-allocation", "digest"] {
        let mut event = harness.failed_before_authentication();
        let attempt = event.attempt.as_mut().unwrap();
        let expected = match changed {
            "foreign" => {
                attempt.capture = harness.original.clone();
                OutboxProblem::Integrity
            }
            "missing" | "missing-allocation" => {
                attempt.capture.observation_id = "not-allocated-in-this-outbox".into();
                if changed == "missing-allocation" {
                    attempt.manifest_sha256 = None;
                }
                OutboxProblem::MissingCapture
            }
            _ => {
                attempt.manifest_sha256 = Some(digest(b"not-the-recorded-manifest"));
                OutboxProblem::Integrity
            }
        };
        let before = harness.episode();
        let revision =
            DiagnosticRevision::with_investigation(&before, before.clone(), event, ANALYZED_AT).unwrap().unwrap();
        harness.reject_with_problem(revision, expected);
    }
}

#[test]
fn allocation_only_history_retains_a_failed_attempt_with_unattestable_manifest_bytes() {
    let mut harness = Harness::new();
    let mut event = harness.failed_before_authentication();
    let attempt = event.attempt.as_mut().unwrap();
    let identity = attempt.capture.clone();
    std::fs::write(harness.files.directory(&identity.observation_id).join("manifest.json"), b"{malformed").unwrap();
    let owner = DiagnosticInvestigationLineage::from_intent(&event.intent).unwrap();
    assert!(harness.files.attempt_receipt(&identity, Some(&owner)).is_err());
    attempt.manifest_sha256 = None;
    let before = harness.episode();
    let mut next = before.clone();
    next.analysis = DiagnosticAnalysisStatus::Deferred;
    next.issue = Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
    let revision = DiagnosticRevision::with_investigation(&before, next, event, ANALYZED_AT).unwrap().unwrap();
    let stored = harness.commit(revision.clone());
    assert_eq!(harness.files.validated(&stored.observation_id).unwrap().diagnostic_revisions, vec![revision]);
    let failed = harness.files.journal.capture(&identity.observation_id).unwrap();
    assert_eq!(
        failed.state,
        OutboxCaptureState::Incomplete,
        "allocation history cannot promote malformed native output"
    );
    assert_eq!(failed.source_work, SourceWorkState::NotStarted);
    assert_eq!(harness.files.journal.debts().unwrap(), 0);
}

#[test]
fn allocation_only_attempts_still_require_the_exact_native_focus_and_admitted_budget() {
    for changed in ["focus", "budget"] {
        let mut harness = Harness::new();
        let mut request = CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::FocusedPageWait { focus: Box::new(harness.intent.focus.clone()) },
            budget: harness.intent.budget.clone(),
        };
        if changed == "focus" {
            let mut row = page_activity();
            row.wait.as_mut().unwrap().identified_resources[0].native = NativeIdentity::new("5:1:43");
            let other = baseline(&mut harness.files, vec![row], true, 10);
            let input = harness.files.diagnostic_input(&other.observation_id, 1).unwrap();
            request.selection = CollectionSelection::FocusedPageWait { focus: Box::new(focus(&input)) };
        } else {
            request.budget.max_queries += 1;
        }
        let reservation = harness.files.reserve("fixture", &request).unwrap();
        let identity = reservation.entry.identity.clone();
        let writer = ObservationFiles::create(&reservation.directory, request, identity.clone(), None).unwrap();
        writer.finish(0).unwrap();
        harness.files.finish(reservation).unwrap();
        let event = DiagnosticInvestigationEvent {
            attempt: Some(DiagnosticAttemptReceipt { capture: identity, manifest_sha256: None }),
            outcome: DiagnosticInvestigationOutcome::Stopped { issue: DiagnosticAnalysisIssue::EvidenceUnavailable },
            ..harness.admitted()
        };
        let before = harness.episode();
        let revision =
            DiagnosticRevision::with_investigation(&before, before.clone(), event, ANALYZED_AT).unwrap().unwrap();
        harness.reject(revision);
    }
}

#[test]
fn captured_events_bind_the_actual_local_capture_scope_and_preserve_pending_native_analysis() {
    let mut harness = Harness::new();
    let event = harness.captured();
    let before = harness.episode();
    let mut forged = event.clone();
    let DiagnosticInvestigationOutcome::Captured { capture } = &mut forged.outcome else { panic!() };
    capture.source_scope_id = digest(b"unrelated-native-source-scope");
    let revision =
        DiagnosticRevision::with_investigation(&before, before.clone(), forged, ANALYZED_AT).unwrap().unwrap();
    harness.reject(revision);
    let revision =
        DiagnosticRevision::with_investigation(&before, before.clone(), event.clone(), ANALYZED_AT).unwrap().unwrap();
    harness.commit(revision);
    assert_eq!(harness.episode().analysis, before.analysis, "captured output alone is not completed analysis");
    let DiagnosticInvestigationOutcome::Captured { capture } = event.outcome else { panic!() };
    assert_eq!(harness.files.diagnostic_input_state(&capture, 1).unwrap(), DiagnosticInputState::Pending);
    assert_eq!(harness.files.journal.debts().unwrap(), 0);
}
