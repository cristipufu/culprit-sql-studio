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
    let target = CollectionTarget::Database(server.clone());
    let mut cases = [
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
    .collect::<Vec<_>>();
    let local = |epoch: &str| SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Node,
            namespace: NativeIdentity::new("private-diagnostic-compute-namespace"),
            native: NativeIdentity::new("private-diagnostic-member"),
        }),
        epoch: Some(epoch.into()),
    };
    let (mut older, mut records) = diagnostic_evidence_partition(target.clone(), local("private-older-epoch"));
    let (newer, new_records) = diagnostic_evidence_partition(target, local("private-newer-epoch"));
    let SourceObservation::Diagnostics(old) = &mut older.observation else { unreachable!() };
    let SourceObservation::Diagnostics(new) = newer.observation else { unreachable!() };
    old.scope_bindings.extend(new.scope_bindings);
    old.records += new.records;
    for record in new_records {
        match record {
            TelemetryRecord::Operational(_) => records.push(record),
            TelemetryRecord::DiagnosticCheckpoint(part) => {
                let Some(TelemetryRecord::DiagnosticCheckpoint(existing)) =
                    records.iter_mut().find(|row| matches!(row, TelemetryRecord::DiagnosticCheckpoint(_)))
                else {
                    unreachable!()
                };
                existing.episodes.extend(part.episodes);
                existing.id = existing.expected_id().unwrap();
                assert!(existing.is_valid());
            }
            _ => unreachable!(),
        }
    }
    cases.push((older, records));
    cases
}

fn diagnostic_evidence(target: CollectionTarget) -> (CollectionDescription, Vec<TelemetryRecord>) {
    diagnostic_evidence_partition(target, SourcePartition::Target)
}

fn diagnostic_evidence_partition(
    target: CollectionTarget,
    partition: SourcePartition,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut baseline, _) = activity_evidence_cases().remove(0);
    baseline.target = target;
    baseline.partition = partition;
    let SourceObservation::Activity(frame) = &mut baseline.observation else { unreachable!() };
    frame.observations = 1;
    frame.native_rows = 1;
    if matches!(&baseline.partition, SourcePartition::InstanceLocal { .. }) {
        frame.epoch = baseline.partition.native_epoch().map(str::to_owned);
    }
    frame.scope =
        if baseline.target.database().is_some() { MeasurementScope::Database } else { MeasurementScope::Server };
    frame.coverage =
        StateSourceCoverage::observed(baseline.source.clone(), 1, frame.scope == MeasurementScope::Database);
    let observed_at = frame.at.clone();
    let mut observation =
        activity_observation(DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected });
    observation.database = baseline.target.database().map(|database| database.database.clone());
    let mut native_capture = fixture().0.capture;
    if matches!(&baseline.partition, SourcePartition::InstanceLocal { .. }) {
        native_capture.observation_id = baseline.captured_range().unwrap().scope_id;
    }
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
    let scope_bindings = [(revision.episode.scope.key().unwrap(), revision.episode.scope.clone())].into();
    let row = TelemetryRecord::Operational(OperationalObservation {
        source: DIAGNOSTICS_SOURCE.into(),
        key: revision.id.clone(),
        fact: OperationalFact::DiagnosticRevision(Box::new(revision)),
    });
    assert!(row.is_valid());
    let source = CollectionDescription {
        partition: culprit_core::telemetry::SourcePartition::Target,
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
            scope_bindings,
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
