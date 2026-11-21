use super::*;
use crate::outbox::DiagnosticUpdate;

impl LocalCaptureFixture {
    pub async fn diagnostic_history(resolved: bool) -> Self {
        let (source, rows) = activity_evidence_cases().remove(0);
        let mut fixture = Self::activity(source, rows).await;
        let outbox = fixture.outbox.as_mut().unwrap();
        let input = outbox
            .diagnostic_input(&fixture.manifest.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
            .await
            .unwrap();
        let original = input.rows.iter().find(|row| matches!(row.row.record, TelemetryRecord::Activity(_))).unwrap();
        let NativeRecordIdentity::Activity(key) = &original.reference.native_key else { unreachable!() };
        let episode = DiagnosticEpisode::start(
            DiagnosticScope::from_source(&input.source).unwrap(),
            DiagnosticSignal::Activity {
                key: key.clone(),
                condition: ActivityCondition::LongRunning,
                policy: ActivityPolicy::default(),
            },
            input.source.observation.activity().unwrap().at.clone(),
            original.reference.clone(),
        )
        .unwrap();
        let revision = DiagnosticRevision::between(None, episode, "2020-01-01T12:00:01Z").unwrap().unwrap();
        let update = history_update(&input, revision, "history-start");
        let mut derived = outbox.commit_diagnostics(input.clone(), update).await.unwrap();
        if resolved {
            let mut empty = input.source.clone();
            let SourceObservation::Activity(frame) = &mut empty.observation else { unreachable!() };
            frame.at = "2020-01-01T12:00:02Z".into();
            frame.observed_from = frame.at.clone();
            frame.observed_until = frame.at.clone();
            frame.native_rows = 0;
            frame.observations = 0;
            frame.coverage = StateSourceCoverage::observed(empty.source.clone(), 0, true);
            let (_, manifest) =
                collect_original(outbox, empty, vec![], CollectionSelection::Activity, SourceShutdown::Quiescent).await;
            let input =
                outbox.diagnostic_input(&manifest.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
            let before = input.checkpoint.as_ref().unwrap().episodes[0].clone();
            let mut next = before.clone();
            next.lifecycle = DiagnosticLifecycle::Resolved;
            next.recovery = Some(DiagnosticRecoveryProof::CompleteScope { capture: input.parent.clone() });
            next.analysis = DiagnosticAnalysisStatus::Unresolved;
            next.issue = Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
            let revision = DiagnosticRevision::between(Some(&before), next, "2020-01-01T12:00:03Z").unwrap().unwrap();
            derived = outbox
                .commit_diagnostics(input.clone(), history_update(&input, revision, "history-resolved"))
                .await
                .unwrap();
            assert!(
                outbox
                    .diagnostic_input(&manifest.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
                    .await
                    .unwrap()
                    .checkpoint
                    .unwrap()
                    .episodes
                    .is_empty(),
                "resolved settled work is absent from the working set but remains in immutable history"
            );
        }
        fixture.capture_directory = fixture.directory.join("captures").join(&derived.observation_id);
        fixture.manifest =
            Outbox::inspect_manifest(&fixture.directory, &derived.observation_id).await.unwrap().unwrap();
        fixture
    }
}
fn history_update(
    input: &crate::outbox::DiagnosticInput,
    revision: DiagnosticRevision,
    step: &str,
) -> DiagnosticUpdate {
    let checkpoint = DiagnosticCheckpoint {
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        target: input.source.target.clone(),
        input: input.parent.clone(),
        episodes: if revision.episode.retained_in_checkpoint() { vec![revision.episode.clone()] } else { vec![] },
    };
    DiagnosticUpdate {
        step_id: digest(step.as_bytes()),
        analyzed_at: revision.analyzed_at.clone(),
        revisions: vec![revision],
        checkpoint,
    }
}
