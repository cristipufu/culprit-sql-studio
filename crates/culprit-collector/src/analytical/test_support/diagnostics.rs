//! Synthetic diagnostic captures preserve private target authority only in recovery, not warehouse Details.
use super::*;
use culprit_core::{DatabaseIdentity, Engine, MeasurementScope};

pub fn diagnostic_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let mut server = DatabaseIdentity::server(
        Engine::Mssql,
        "private-diagnostic-database",
        "private-diagnostic-host.internal",
        54339,
        Some("private-diagnostic-instance".into()),
    );
    server.incarnation = Some("private-diagnostic-incarnation".into());
    [
        server.into(),
        DatabaseIdentity::file("private-diagnostic-file", "/private/diagnostics/fixture.sqlite".into()).into(),
        CollectionTarget::Shared(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("private-diagnostic-namespace"),
            native: NativeIdentity::new("private-diagnostic-native-id")
                .qualified("host", "private-diagnostic-native-host"),
        }),
    ]
    .into_iter()
    .map(diagnostic_evidence)
    .collect()
}

fn diagnostic_evidence(target: CollectionTarget) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut baseline, _) = activity_evidence_cases().remove(0);
    baseline.target = target;
    let SourceObservation::Activity(frame) = &mut baseline.observation else { unreachable!() };
    frame.observations = 1;
    frame.native_rows = 1;
    frame.scope =
        if baseline.target.database().is_some() { MeasurementScope::Database } else { MeasurementScope::Server };
    frame.coverage =
        StateSourceCoverage::observed(baseline.source.clone(), 1, frame.scope == MeasurementScope::Database);
    let observed_at = frame.at.clone();
    let mut observation =
        activity_observation(DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected });
    observation.database = baseline.target.database().map(|database| database.database.clone());
    let native_capture = fixture().0.capture;
    let original = ExportedRecord::new(&baseline, &native_capture, TelemetryRecord::from(observation.clone())).unwrap();
    let reference =
        DiagnosticEvidenceRef::from_export(&baseline, &digest(b"original-private-manifest"), &original, 0).unwrap();
    let parent = reference.capture_ref();
    let episode = DiagnosticEpisode::start(
        DiagnosticScope::from_source(&baseline).unwrap(),
        DiagnosticSignal::Activity {
            key: observation.key,
            condition: ActivityCondition::LongRunning,
            policy: ActivityPolicy::default(),
        },
        observed_at,
        reference,
    )
    .unwrap();
    let analyzed_at = "2020-01-01T12:00:01.000000000Z";
    let revision = DiagnosticRevision::between(None, episode, analyzed_at).unwrap().unwrap();
    let mut checkpoint = DiagnosticCheckpointPart {
        id: String::new(),
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        target: baseline.target.clone(),
        input: parent.clone(),
        part: 0,
        parts: 1,
        episodes: vec![revision.episode.clone()],
    };
    checkpoint.id = checkpoint.expected_id().unwrap();
    assert!(checkpoint.is_valid());
    let row = TelemetryRecord::Operational(OperationalObservation {
        source: DIAGNOSTICS_SOURCE.into(),
        key: revision.id.clone(),
        fact: OperationalFact::DiagnosticRevision(Box::new(revision)),
    });
    assert!(row.is_valid());
    let source = CollectionDescription {
        target: baseline.target,
        source: DIAGNOSTICS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Diagnostics(DiagnosticCaptureDescription {
            controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
            parent,
            step_id: digest(b"private-diagnostic-step"),
            analyzed_at: analyzed_at.into(),
            records: 1,
            checkpoint_records: 1,
        }),
    };
    (source, vec![row, TelemetryRecord::DiagnosticCheckpoint(checkpoint)])
}

pub fn assert_diagnostic_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    assert_eq!(capture.source(), source);
    let expected = records
        .iter()
        .filter(|row| row.stream() == TelemetryStream::OperationalSamples)
        .map(|row| ExportedRecord::new(source, capture.capture(), row.clone()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(capture.records().len(), expected.len());
    for row in expected {
        assert!(
            capture.records().iter().any(|record| record.row() == &row),
            "restore the exact private target and original native receipt"
        );
    }
}
