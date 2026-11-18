use super::*;

fn captured() -> CachedRuntimeRead {
    let mut read = read();
    let capture = CaptureIdentity {
        observation_id: "a".repeat(32),
        revision: Some(CaptureRevision { outbox_id: "b".repeat(32), owner_epoch: "1".into(), sequence: "2".into() }),
    };
    let parent = DiagnosticCaptureRef {
        capture: capture.clone(),
        manifest_sha256: digest(b"original-manifest"),
        source_scope_id: read.description.captured_range().unwrap().scope_id,
    };
    let exported =
        ExportedRecord::new(&read.description, &capture, TelemetryRecord::Operational(read.observations[0].clone()))
            .unwrap();
    let positions = vec![CapturedSnapshotPosition::from_export(&exported, 0).unwrap()];
    read.origin = CachedRuntimeReadOrigin::captured(&read.description, parent, positions).unwrap();
    assert!(read.is_valid());
    read
}

#[test]
fn captured_origin_reconstructs_the_exact_original_receipt_without_a_native_position_or_duplicate_owner_id() {
    let original = captured();
    let CachedRuntimeReadOrigin::Captured { capture, .. } = &original.origin else { panic!() };
    let references = original.captured_references().unwrap().unwrap();
    assert_eq!(references[0].capture_ref(), *capture);
    assert_eq!(references[0].row_ordinal, 0);
    references[0]
        .validate_canonical_record(
            &original.description,
            &TelemetryRecord::Operational(original.observations[0].clone()),
        )
        .unwrap();
    assert_eq!(original.read_id().unwrap(), cached_statement_read_id(&capture.capture).unwrap());
    let encoded = serde_json::to_value(&original).unwrap();
    assert!(encoded.get("read_id").is_none());
    assert_eq!(encoded["origin"]["kind"], "captured");
    assert!(encoded["origin"].get("read_id").is_none());
    let decoded: CachedRuntimeRead = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(decoded, original);
    assert_eq!(decoded.captured_references().unwrap(), Some(references));
    let mut mixed = encoded.clone();
    mixed["origin"]["read_id"] = serde_json::Value::String("c".repeat(64));
    assert!(serde_json::from_value::<CachedRuntimeRead>(mixed).is_err());
    let mut unknown = encoded;
    unknown["origin"]["kind"] = serde_json::Value::String("unrecorded-read".into());
    assert!(serde_json::from_value::<CachedRuntimeRead>(unknown).is_err());
}

#[test]
fn canonical_counter_byte_changes_cannot_be_attached_to_an_original_manifest_receipt() {
    let mut changed = captured();
    let OperationalFact::MssqlCachedRuntime(runtime) = &mut changed.observations[0].fact else { panic!() };
    runtime.counters.get_mut(&CachedRuntimeMetric::SpilledPages).unwrap().total = "9007199254740994".into();
    assert!(changed.observations[0].is_valid(), "a valid changed counter still is a different original body");
    assert!(!changed.is_valid());
    assert!(changed.captured_references().is_err());
}

#[test]
fn original_source_scope_clocks_coverage_and_row_positions_cannot_be_rebound() {
    for change in ["epoch", "database", "clock", "coverage", "ordinal", "digest", "owner", "scope"] {
        let mut changed = captured();
        match change {
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.description.partition else { panic!() };
                *epoch = Some("replacement-process".into());
            }
            "database" => {
                let CollectionTarget::Database(database) = &mut changed.description.target else { panic!() };
                database.database = "other-db".into();
            }
            "clock" => {
                let SourceObservation::Snapshot(header) = &mut changed.description.observation else { panic!() };
                header.observed_until = "2020-01-01T12:00:02Z".into();
            }
            "coverage" => {
                let SourceObservation::Snapshot(header) = &mut changed.description.observation else { panic!() };
                header.sources[0] = StateSourceCoverage::observed(header.sources[0].source.clone(), 1, false);
            }
            _ => {
                let CachedRuntimeReadOrigin::Captured { capture, positions, .. } = &mut changed.origin else {
                    panic!()
                };
                match change {
                    "ordinal" => positions[0].row_ordinal = 1,
                    "digest" => positions[0].payload_sha256 = digest(b"another-row"),
                    "owner" => capture.capture.revision.as_mut().unwrap().owner_epoch = "2".into(),
                    _ => capture.source_scope_id = digest(b"another-scope"),
                }
            }
        }
        assert!(!changed.is_valid(), "{change}");
        assert!(changed.captured_references().is_err(), "{change}");
    }
}

#[test]
fn a_missing_captured_body_preserves_original_counts_and_an_explicit_read_gap() {
    let mut missing = captured();
    missing.observations.clear();
    let CachedRuntimeReadOrigin::Captured { positions, .. } = &mut missing.origin else { panic!() };
    positions.clear();
    assert!(!missing.is_valid());
    missing.read_complete = false;
    assert!(missing.is_valid());
    assert_eq!(missing.description.observation.snapshot().unwrap().records, 1);
    assert_eq!(missing.captured_references().unwrap(), Some(vec![]));
}

#[test]
fn duplicate_positions_cannot_pool_two_native_rows_into_one_captured_ordinal() {
    let mut changed = captured();
    let mut next = changed.observations[0].clone();
    let OperationalFact::MssqlCachedRuntime(runtime) = &mut next.fact else { panic!() };
    runtime.context.identity.plan_generation = "2".into();
    next.key = runtime.context.identity.key().unwrap();
    changed.observations.push(next);
    let SourceObservation::Snapshot(header) = &mut changed.description.observation else { panic!() };
    header.records = 2;
    header.sources[0].records = 2;
    let CachedRuntimeReadOrigin::Captured { capture, .. } = &changed.origin else { panic!() };
    let positions = changed
        .observations
        .iter()
        .map(|row| {
            let exported =
                ExportedRecord::new(&changed.description, &capture.capture, TelemetryRecord::Operational(row.clone()))
                    .unwrap();
            CapturedSnapshotPosition::from_export(&exported, 0).unwrap()
        })
        .collect();
    changed.origin = CachedRuntimeReadOrigin::captured(&changed.description, capture.clone(), positions).unwrap();
    assert!(CachedRuntimeRead::accepts_rows(&changed.description, &changed.observations, true));
    assert!(!changed.is_valid());
    assert!(changed.captured_references().is_err());
}

#[test]
fn sharing_withholds_the_entire_original_proof_carrier_and_keeps_the_explicit_gap_idempotently() {
    let original = snapshot(captured());
    let before = serde_json::to_vec(&original.operations.cached_runtime).unwrap();
    let shared = crate::redact::redact_snapshot(&original);
    assert!(shared.operations.cached_runtime.is_none());
    let gap = shared.sources.iter().find(|source| source.source == CACHED_RUNTIME_READ_SOURCE).unwrap();
    assert_eq!(gap.state, crate::SourceState::Unavailable);
    let encoded = serde_json::to_string(&shared).unwrap();
    for private in ["manifest_sha256", "source_sha256", "payload_sha256", "row_ordinals", "owned-process-epoch"] {
        assert!(!encoded.contains(private), "proof field escaped sharing: {private}");
    }
    let repeated = crate::redact::redact_snapshot(&shared);
    assert!(repeated.operations.cached_runtime.is_none());
    assert_eq!(
        repeated.sources.iter().find(|source| source.source == CACHED_RUNTIME_READ_SOURCE).unwrap().state,
        crate::SourceState::Unavailable
    );
    assert_eq!(before, serde_json::to_vec(&original.operations.cached_runtime).unwrap());
}
