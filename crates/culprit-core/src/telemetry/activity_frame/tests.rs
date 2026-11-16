use super::*;
use crate::{DatabaseIdentity, Engine, MeasurementScope};

pub(crate) fn request(id: &str) -> ActivityObservation {
    super::super::activity::tests::request(id)
}

pub(crate) fn source(rows: usize) -> CollectionDescription {
    CollectionDescription {
        target: DatabaseIdentity::server(Engine::Mssql, "sample", "fixture.invalid", 1433, None).into(),
        partition: SourcePartition::Target,
        source: "original-activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some("original-native-epoch".into()),
            database_writable: Some(true),
            observed_from: "2026-11-13T10:00:00Z".into(),
            observed_until: "2026-11-13T10:00:00.1Z".into(),
            at: "2026-11-13T10:00:00.05Z".into(),
            scope: MeasurementScope::Database,
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            native_rows: rows as u32,
            observations: rows as u32,
            coverage: StateSourceCoverage::collected("original-activity".into(), rows as u32),
        }),
    }
}

pub(crate) fn references(source: &CollectionDescription, rows: &[ActivityObservation]) -> Vec<DiagnosticEvidenceRef> {
    let capture = CaptureIdentity {
        observation_id: "original-activity-capture".into(),
        revision: Some(CaptureRevision {
            outbox_id: digest(b"original activity fixture"),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    rows.iter()
        .enumerate()
        .map(|(ordinal, row)| {
            let exported = ExportedRecord::new(source, &capture, TelemetryRecord::from(row.clone())).unwrap();
            DiagnosticEvidenceRef::from_export(source, &digest(b"original manifest"), &exported, ordinal as u32)
                .unwrap()
        })
        .collect()
}

pub(crate) fn borrowed<'a>(
    rows: &'a [ActivityObservation],
    refs: &'a [DiagnosticEvidenceRef],
) -> Vec<ActivityFrameRow<'a>> {
    rows.iter()
        .zip(refs)
        .map(|(observation, reference)| ActivityFrameRow { observation, reference: Some(reference) })
        .collect()
}

#[test]
fn one_session_index_preserves_all_request_contexts_and_idle_context_without_representatives() {
    let first = super::super::activity::tests::request("42");
    let mut mars = first.clone();
    mars.key.request.as_mut().unwrap().id = "1".into();
    let mut idle = first.clone();
    idle.key.session.id = "43".into();
    idle.key.request = None;
    idle.request_counters = None;
    idle.idle = true;
    let rows = [first.clone(), mars.clone(), idle.clone()];
    let source = source(rows.len());
    let refs = references(&source, &rows);
    let frame = ActivityFrameEvidence::new(&source, &borrowed(&rows, &refs), true).unwrap();
    assert_eq!(
        frame.session_rows(&first.key.session).map(|row| &row.observation.key).collect::<Vec<_>>(),
        [&first.key, &mars.key]
    );
    assert_eq!(frame.session_rows(&idle.key.session).next().unwrap().observation, &idle);
    assert_eq!(frame.row(&mars.key).unwrap().reference, Some(&refs[1]));
}

#[test]
fn duplicate_keys_or_reused_native_session_generations_cannot_become_a_frame() {
    let original = super::super::activity::tests::request("42");
    for changed in [original.clone(), {
        let mut changed = original.clone();
        changed.key.session.generation = "another-login".into();
        changed
    }] {
        let rows = [original.clone(), changed];
        let source = source(rows.len());
        let borrowed =
            rows.iter().map(|observation| ActivityFrameRow { observation, reference: None }).collect::<Vec<_>>();
        assert!(ActivityFrameEvidence::new(&source, &borrowed, true).is_err());
    }
}

#[test]
fn read_gaps_preserve_original_counts_but_never_accept_a_false_complete_read() {
    let row = super::super::activity::tests::request("42");
    let source = source(2);
    let refs = references(&source, std::slice::from_ref(&row));
    let borrowed = borrowed(std::slice::from_ref(&row), &refs);
    assert!(ActivityFrameEvidence::new(&source, &borrowed, true).is_err());
    let frame = ActivityFrameEvidence::new(&source, &borrowed, false).unwrap();
    assert!(!frame.read_complete());
    assert_eq!(frame.native().observations, 2);
    assert_eq!(frame.native().coverage.state, crate::SourceState::Collected);
}

#[test]
fn original_header_and_complete_record_receipts_are_validated_without_authenticating_storage() {
    let rows = [super::super::activity::tests::request("42"), super::super::activity::tests::request("43")];
    let source = source(rows.len());
    let refs = references(&source, &rows);
    for field in ["scope", "manifest", "capture", "payload", "ordinal", "native-key"] {
        let mut changed = refs.clone();
        match field {
            "scope" => changed[1].source_scope_id = digest(b"another scope"),
            "manifest" => changed[1].manifest_sha256 = digest(b"another manifest"),
            "capture" => changed[1].capture.observation_id = "another-capture".into(),
            "payload" => changed[1].payload_sha256 = digest(b"another payload"),
            "ordinal" => changed[1].row_ordinal = 0,
            _ => changed[1].native_key = refs[0].native_key.clone(),
        }
        assert!(ActivityFrameEvidence::new(&source, &borrowed(&rows, &changed), true).is_err(), "{field}");
    }
    for field in ["database", "bounds", "source", "epoch"] {
        let mut changed = source.clone();
        let SourceObservation::Activity(native) = &mut changed.observation else { unreachable!() };
        match field {
            "database" => {
                changed.target = DatabaseIdentity::server(Engine::Mssql, "other", "fixture.invalid", 1433, None).into()
            }
            "bounds" => native.observed_until = "2026-11-13T09:00:00Z".into(),
            "source" => changed.source = "another-source".into(),
            _ => changed.partition = SourcePartition::InstanceLocal { member: None, epoch: Some("different".into()) },
        }
        assert!(ActivityFrameEvidence::new(&changed, &borrowed(&rows, &refs), true).is_err(), "{field}");
    }
    let mixed = [
        ActivityFrameRow { observation: &rows[0], reference: Some(&refs[0]) },
        ActivityFrameRow { observation: &rows[1], reference: None },
    ];
    assert!(ActivityFrameEvidence::new(&source, &mixed, true).is_err());
}
