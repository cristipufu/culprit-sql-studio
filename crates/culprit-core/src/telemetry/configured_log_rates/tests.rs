use super::*;

fn point() -> ConfiguredLogRateLimits {
    ConfiguredLogRateLimits {
        observed_at: Some(canonical_time("2020-01-01T12:00:00Z").unwrap()),
        database_id: "7".into(),
        database_incarnation: "original-logical-guid".into(),
        workload_group_id: Some("12".into()),
        workload_group_bytes_per_second: Some("9007199254740993".into()),
        resource_pool_bytes_per_second: Some("0".into()),
        instance_bytes_per_second: None,
    }
}

fn source() -> CollectionDescription {
    let mut target = DatabaseIdentity::server(crate::Engine::Mssql, "app", "fixture.invalid", 1433, None);
    target.incarnation = Some("original-logical-guid".into());
    CollectionDescription {
        target: target.into(),
        partition: SourcePartition::Target,
        source: "operational-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::observed(CONFIGURED_LOG_RATES_SOURCE.into(), 1, false)],
        }),
    }
}

#[test]
fn exact_native_rates_keep_zero_null_and_distinct_scopes_without_approximation() {
    let original = point();
    assert!(original.is_valid());
    let restored: ConfiguredLogRateLimits = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(restored, original);
    for invalid in ["-1", "01", "+1", "1.0", "9223372036854775808"] {
        let mut changed = original.clone();
        changed.workload_group_bytes_per_second = Some(invalid.into());
        assert!(!changed.is_valid());
    }
}

#[test]
fn canonical_receipts_reject_foreign_incarnations_clocks_and_changed_native_limits() {
    let source = source();
    let original = TelemetryRecord::Operational(OperationalObservation {
        source: CONFIGURED_LOG_RATES_SOURCE.into(),
        key: point().key().unwrap(),
        fact: OperationalFact::ConfiguredLogRateLimits(Box::new(point())),
    });
    let capture = CaptureIdentity {
        observation_id: "configured-limits-original".into(),
        revision: Some(CaptureRevision {
            outbox_id: "configured-owner".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let exported = ExportedRecord::new(&source, &capture, original).unwrap();
    let reference = DiagnosticEvidenceRef::from_export(&source, &digest(b"original manifest"), &exported, 0).unwrap();
    reference.validate_canonical_record(&source, &exported.record).unwrap();
    let mut changed = exported.record.clone();
    let TelemetryRecord::Operational(row) = &mut changed else { panic!() };
    let OperationalFact::ConfiguredLogRateLimits(changed_point) = &mut row.fact else { panic!() };
    changed_point.resource_pool_bytes_per_second = Some("1".into());
    assert!(reference.validate_canonical_record(&source, &changed).is_err());
    for (incarnation, at) in
        [("recreated-native-database", "2020-01-01T12:00:00Z"), ("original-logical-guid", "2020-01-01T12:00:02Z")]
    {
        let mut point = point();
        point.database_incarnation = incarnation.into();
        point.observed_at = Some(canonical_time(at).unwrap());
        assert!(!point.fits_source(&source));
    }
}

#[test]
fn missing_archived_body_keeps_original_count_and_cannot_be_a_complete_empty_read() {
    let source = source();
    let read = ConfiguredLogRateRead::from_source(&source, None).unwrap();
    assert!(read.is_valid_read(false));
    assert!(!read.is_valid_read(true));
    assert_eq!(read.source.records, 1);
    assert!(read.observation.is_none());
    let mut changed = read;
    changed.source =
        StateSourceCoverage::unavailable(CONFIGURED_LOG_RATES_SOURCE.into(), crate::ErrorCode::PermissionDenied, None);
    changed.observation = Some(Box::new(point()));
    assert!(!changed.is_valid_read(false));
}
