//! Projection checks immutable proof from the canonical read; it never reopens files or reissues positions.
use super::*;

fn change_counter(snapshot: &mut CompletedSnapshot) {
    let TelemetryRecord::Operational(row) = &mut snapshot.records[0].record else { panic!() };
    let OperationalFact::MssqlCachedRuntime(point) = &mut row.fact else { panic!() };
    point.executions = Some("9007199254740994".into());
    assert!(point.is_valid(), "the replacement is valid native-shaped data, not malformed noise");
}

#[tokio::test]
async fn valid_changed_bodies_cannot_become_new_references_or_native_runtime_inputs() {
    let fixture = Fixture::new(1, false).await;
    let original = fixture.read(CompletedCaptureBudget::default()).await.unwrap();
    let reference = original.reference(0).unwrap();
    assert_eq!(reference.payload_sha256, original.records()[0].payload_sha256().unwrap());
    let carrier = original.into_cached_runtime_read().unwrap();
    assert_eq!(carrier.captured_references().unwrap(), Some(vec![reference]));

    let mut changed = fixture.read(CompletedCaptureBudget::default()).await.unwrap();
    let original_digest = changed.original.positions[0].payload_sha256.clone();
    change_counter(&mut changed);
    assert_ne!(changed.records[0].payload_sha256().unwrap(), original_digest);
    assert!(changed.reference(0).is_err());
    assert!(changed.into_cached_runtime_read().is_err());
}

#[tokio::test]
async fn reordered_originals_in_bounds_ordinal_changes_and_export_envelope_changes_never_resign_proof() {
    let fixture = Fixture::new(2, false).await;
    for change in ["order", "ordinal", "envelope", "source"] {
        let mut changed = fixture.read(CompletedCaptureBudget::default()).await.unwrap();
        match change {
            "order" => changed.records.swap(0, 1),
            "ordinal" => changed.row_ordinals.swap(0, 1),
            "envelope" => changed.records[0].batch_id = digest(b"different batch"),
            "source" => changed.source.source = "different-original-reader".into(),
            _ => unreachable!(),
        }
        assert!(changed.reference(0).is_err(), "{change}");
        assert!(changed.into_cached_runtime_read().is_err(), "{change}");
    }
}

#[tokio::test]
async fn opaque_completed_original_survives_later_file_rewrite_but_the_next_read_quarantines_it() {
    let fixture = Fixture::new(1, false).await;
    let completed = fixture.read(CompletedCaptureBudget::default()).await.unwrap();
    let reference = completed.reference(0).unwrap();
    let bytes = completed.bytes_read();
    let body = std::fs::read_to_string(fixture.body()).unwrap();
    let changed = body.replace("9007199254740993", "9007199254740994");
    assert_ne!(body, changed);
    assert_eq!(body.len(), changed.len());
    std::fs::write(fixture.body(), changed).unwrap();
    assert_eq!(completed.bytes_read(), bytes);
    let read = completed.into_cached_runtime_read().unwrap();
    assert_eq!(read.captured_references().unwrap(), Some(vec![reference]));
    let OperationalFact::MssqlCachedRuntime(point) = &read.observations[0].fact else { panic!() };
    assert_eq!(point.executions.as_deref(), Some("9007199254740993"));
    assert!(fixture.read(CompletedCaptureBudget::default()).await.is_err());
    assert_eq!(fixture.state().await, OutboxCaptureState::Quarantined);
}
