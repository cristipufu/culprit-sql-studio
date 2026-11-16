//! Synthetic sealed native evidence exercises the actual local CAS writer, never a native cost qualification.
use super::*;
use crate::analytical::test_support::{
    focused_request_memory_evidence_with_peers, request_memory_investigation_fixture,
};

struct MemoryInvestigation {
    _root: tempfile::TempDir,
    files: Files,
    input: DiagnosticInput,
}
impl MemoryInvestigation {
    fn new(peers: usize) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
        let fixture = request_memory_investigation_fixture();
        let baseline = seal(
            &mut files,
            fixture.baseline_source,
            CollectionSelection::Activity,
            vec![fixture.baseline_record.record],
        );
        let input = files.diagnostic_input(&baseline.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
        let activity = input
            .rows
            .iter()
            .map(|row| match &row.row.record {
                TelemetryRecord::Activity(observation) => {
                    DiagnosticActivityRow { reference: &row.reference, observation }
                }
                _ => panic!(),
            })
            .collect::<Vec<_>>();
        let observed = observe_diagnostic_activity(
            &input.source,
            &input.parent,
            &activity,
            None,
            &ActivityPolicy::default(),
            "2020-01-01T12:00:01Z",
        )
        .unwrap();
        files
            .commit_diagnostics(
                input.clone(),
                DiagnosticUpdate {
                    step_id: digest(b"native memory baseline"),
                    analyzed_at: "2020-01-01T12:00:01Z".into(),
                    checkpoint: observed.checkpoint,
                    revisions: observed.revisions,
                },
            )
            .unwrap();
        let (mut source, records) = focused_request_memory_evidence_with_peers(peers);
        let SourceObservation::FocusedRequestMemory { result, .. } = &mut source.observation else { panic!() };
        result.focus.parent = input.rows[0].reference.clone();
        let selection = CollectionSelection::FocusedRequestMemory { focus: Box::new(result.focus.clone()) };
        let captured = seal(&mut files, source, selection, records);
        let input = files.diagnostic_input(&captured.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
        Self { _root: root, files, input }
    }

    fn proposal(&self) -> DiagnosticUpdate {
        let rows = self
            .input
            .rows
            .iter()
            .map(|row| match &row.row.record {
                TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::ExecutionGrant(observation),
                    ..
                }) => DiagnosticMemoryRow { reference: &row.reference, observation },
                _ => panic!(),
            })
            .collect::<Vec<_>>();
        let update = analyze_diagnostic_memory(
            &self.input.source,
            &self.input.parent,
            &rows,
            self.input.checkpoint.as_ref().unwrap(),
            "2020-01-01T12:00:02Z",
            None,
        )
        .unwrap();
        assert_eq!(update.revisions.len(), 1);
        DiagnosticUpdate {
            step_id: digest(b"native memory follow-up"),
            analyzed_at: "2020-01-01T12:00:02Z".into(),
            revisions: update.revisions,
            checkpoint: update.checkpoint,
        }
    }
}

fn seal(
    files: &mut Files,
    source: CollectionDescription,
    selection: CollectionSelection,
    records: Vec<TelemetryRecord>,
) -> CaptureIdentity {
    let request =
        CollectionRequest { subject: CollectionSubject::Database, selection, budget: CollectionBudget::default() };
    let reservation = files.reserve("fixture", &request).unwrap();
    let identity = reservation.entry.identity.clone();
    files.journal.source_work(&reservation.entry, SourceWorkState::Active).unwrap();
    let mut writer = ObservationFiles::create(&reservation.directory, request, identity.clone(), None).unwrap();
    writer.describe(source).unwrap();
    writer.begin_page(0).unwrap();
    assert!(writer.write_page(0, &records).unwrap().is_none());
    writer.seal(0).unwrap();
    writer.confirm_source(SourceCompletion::Verified).unwrap();
    assert_eq!(writer.finish(0).unwrap().status, CollectionStatus::Complete);
    assert!(std::fs::metadata(reservation.directory.join("manifest.json")).unwrap().len() <= 128 * 1024);
    files.journal.source_work(&reservation.entry, SourceWorkState::Quiescent).unwrap();
    files.finish(reservation).unwrap();
    identity
}

#[test]
fn all_two_hundred_original_native_rows_can_enter_one_bounded_manifest_and_immutable_history() {
    let mut fixture = MemoryInvestigation::new(199);
    let update = fixture.proposal();
    let revision = &update.revisions[0];
    assert_eq!(revision.episode.analysis, DiagnosticAnalysisStatus::Analyzed);
    assert_eq!(revision.episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(revision.episode.report.as_ref().unwrap().references().count(), 200);
    assert!(revision.is_valid() && revision.inputs.len() <= MAX_DIAGNOSTIC_INPUTS);
    let expected = revision.episode.report.clone();
    fixture.files.commit_diagnostics(fixture.input.clone(), update).unwrap();
    let restored = fixture
        .files
        .diagnostic_input(&fixture.input.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .unwrap();
    assert_eq!(restored.checkpoint.as_ref().unwrap().episodes[0].report, expected);
    let pinned =
        super::super::journal::checkpoint_references(restored.checkpoint.as_ref().unwrap()).collect::<Vec<_>>();
    assert!(expected.as_ref().unwrap().references().all(|reference| pinned.contains(&reference)));
    assert_eq!(fixture.files.journal.debts().unwrap(), 0);
}

#[test]
fn an_omitted_or_substituted_peer_cannot_be_committed_as_a_complete_analysis() {
    for substitution in [false, true] {
        let mut fixture = MemoryInvestigation::new(2);
        let mut update = fixture.proposal();
        let revision = &mut update.revisions[0];
        let DiagnosticReport::RequestMemory { peers, .. } = revision.episode.report.as_mut().unwrap() else { panic!() };
        if substitution {
            peers[0].payload_sha256 = digest(b"substituted peer native payload");
        } else {
            peers.pop();
        }
        for reference in revision.episode.report.as_ref().unwrap().references() {
            if !revision.inputs.contains(reference) {
                revision.inputs.push(reference.clone());
            }
        }
        revision.id = revision.expected_id().unwrap();
        assert!(revision.is_valid(), "the forged proposal reaches the local authenticated-input guard");
        update.checkpoint.episodes = vec![revision.episode.clone()];
        let before = fixture.input.checkpoint.clone();
        let error = fixture.files.commit_diagnostics(fixture.input.clone(), update).unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Outbox(OutboxProblem::Integrity)));
        let current = fixture
            .files
            .diagnostic_input(&fixture.input.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
            .unwrap();
        assert_eq!(current.checkpoint, before);
        assert_eq!(fixture.files.journal.debts().unwrap(), 0);
    }
}
