use super::*;
use culprit_core::{DatabaseIdentity, Engine, telemetry::*};
fn read(id: &str) -> CapturedOperationalRead {
    let source = CollectionDescription {
        target: DatabaseIdentity::server(Engine::Mssql, "db", "original.invalid", 1433, None).into(),
        partition: SourcePartition::Target,
        source: "unit-original".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T00:00:00Z".into(),
            observed_until: "2020-01-01T00:00:01Z".into(),
            records: 0,
            sources: vec![StateSourceCoverage::observed("unit-original".into(), 0, true)],
        }),
    };
    let capture = DiagnosticCaptureRef {
        capture: CaptureIdentity {
            observation_id: id.into(),
            revision: Some(CaptureRevision {
                outbox_id: "original-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        },
        manifest_sha256: digest(b"original-manifest"),
        source_scope_id: source.captured_range().unwrap().scope_id,
    };
    CapturedOperationalRead::captured(source, capture, Vec::new(), Vec::new(), true).unwrap()
}
fn interpreted(original: &RecordedOperationalRead) -> Result<usize> {
    match original {
        RecordedOperationalRead::Captured { read } if read.is_valid() => Ok(1),
        _ => Ok(0),
    }
}
fn unavailable(_: &Message) -> usize {
    0
}
fn run(originals: &[RecordedOperationalRead]) -> Vec<usize> {
    explain(originals, interpreted, unavailable, culprit_core::message!("view.table_memory.invalid"), |shown, total| {
        culprit_core::message!(
            "view.table_memory.presentation_limited",
            shown = shown.to_string(),
            total = total.to_string()
        )
    })
    .unwrap()
}
#[test]
fn imported_exact_repeats_are_idempotent_and_explicit_unavailable_markers_remain_independent() {
    let captured = RecordedOperationalRead::Captured { read: Box::new(read("original")) };
    let marker =
        RecordedOperationalRead::Unavailable { reason: culprit_core::message!("view.table_memory.sharing_withheld") };
    let raw = vec![captured.clone(), marker.clone(), captured, marker];
    let raw: Vec<RecordedOperationalRead> = serde_json::from_slice(&serde_json::to_vec(&raw).unwrap()).unwrap();
    assert_eq!(run(&raw), vec![1, 0, 0]);
}
#[test]
fn conflicts_after_the_presentation_cap_withdraw_every_measured_copy_and_preserve_raw_overflow() {
    let first = read("original");
    let mut contradictory = first.clone();
    contradictory.origin.capture.manifest_sha256 = digest(b"other-manifest");
    assert!(contradictory.is_valid());
    let marker =
        RecordedOperationalRead::Unavailable { reason: culprit_core::message!("view.table_memory.sharing_withheld") };
    let mut raw = vec![RecordedOperationalRead::Captured { read: Box::new(first) }];
    raw.extend(std::iter::repeat_n(marker, MAX_RECORDED_OPERATIONAL_READS + 1));
    raw.push(RecordedOperationalRead::Captured { read: Box::new(contradictory) });
    let before = serde_json::to_value(&raw).unwrap();
    let output = run(&raw);
    assert_eq!(output.len(), MAX_RECORDED_OPERATIONAL_READS + 1);
    assert!(output.iter().all(|value| *value == 0));
    assert_eq!(serde_json::to_value(&raw).unwrap(), before);
    assert_eq!(run(&raw), output);
}
#[test]
fn a_family_refusal_never_mutates_the_raw_original_or_creates_another_identity_free_marker() {
    fn refuse(_: &RecordedOperationalRead) -> Result<usize> {
        Err(CollectionStop::InvalidPage.error())
    }
    let raw = vec![RecordedOperationalRead::Captured { read: Box::new(read("original")) }];
    let before = serde_json::to_value(&raw).unwrap();
    let output = explain(&raw, refuse, unavailable, culprit_core::message!("view.table_memory.invalid"), |_, _| {
        culprit_core::message!("view.table_memory.invalid")
    })
    .unwrap();
    assert_eq!(output, vec![0]);
    assert_eq!(serde_json::to_value(&raw).unwrap(), before);
}
