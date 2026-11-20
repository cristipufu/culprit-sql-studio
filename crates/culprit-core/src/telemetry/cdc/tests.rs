use super::*;
fn point() -> MssqlCdcObservation {
    MssqlCdcObservation {
        context: super::super::native_read_context::tests::read_context(),
        database_id: 5,
        observed_at: "2020-01-01T12:00:00.500Z".into(),
        position: 1,
        reset: crate::ResetMarker::Unknown,
        data: MssqlCdcData::Status(MssqlCdcStatus {
            native_major: 16,
            native_edition: 3,
            enabled: Some(true),
            permission: MssqlCdcPermission::ViewDatabasePerformanceState,
            has_permission: Some(true),
            scan_layout_available: true,
            error_layout_available: true,
        }),
    }
}
fn description(point: &MssqlCdcObservation) -> CollectionDescription {
    CollectionDescription {
        target: point.context.target.clone(),
        partition: point.context.partition.clone(),
        source: "mssql-cdc".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Cdc,
            observed_from: point.context.observed_from.clone(),
            observed_until: point.context.observed_until.clone(),
            records: 1,
            sources: [MSSQL_CDC_STATUS_SOURCE, MSSQL_CDC_SCANS_SOURCE, MSSQL_CDC_ERRORS_SOURCE]
                .into_iter()
                .map(|name| {
                    if name == point.source() {
                        StateSourceCoverage::collected(name.into(), 1)
                    } else {
                        StateSourceCoverage::unavailable(name.into(), crate::ErrorCode::QueryFailed, None)
                    }
                })
                .collect(),
        }),
    }
}
fn row(point: MssqlCdcObservation) -> TelemetryRecord {
    TelemetryRecord::Operational(OperationalObservation {
        source: point.source().into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MssqlCdc(Box::new(point)),
    })
}
#[test]
fn native_permission_boundary_and_null_status_remain_explicit() {
    assert_eq!(MssqlCdcPermission::for_major(15), MssqlCdcPermission::ViewDatabaseState);
    assert_eq!(MssqlCdcPermission::for_major(16), MssqlCdcPermission::ViewDatabasePerformanceState);
    let mut point = point();
    let MssqlCdcData::Status(status) = &mut point.data else { unreachable!() };
    status.enabled = None;
    status.has_permission = None;
    assert!(point.is_valid());
    let restored: MssqlCdcObservation = serde_json::from_value(serde_json::to_value(&point).unwrap()).unwrap();
    assert_eq!(restored, point);
    let MssqlCdcData::Status(status) = &mut point.data else { unreachable!() };
    status.permission = MssqlCdcPermission::ViewDatabaseState;
    assert!(!point.is_valid(), "the old permission must not authorize a 2022 native source");
}
#[test]
fn raw_export_binds_vendor_dataset_original_owner_and_native_bounds() {
    let original = point();
    let source = description(&original);
    let capture = CaptureIdentity { observation_id: "cdc-original".into(), revision: None };
    assert!(ExportedRecord::new(&source, &capture, row(original.clone())).is_ok());
    for change in ["dataset", "member", "bounds"] {
        let mut foreign = source.clone();
        match change {
            "dataset" => {
                let SourceObservation::Snapshot(header) = &mut foreign.observation else { unreachable!() };
                header.dataset = SnapshotDataset::Cleanup;
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut foreign.partition else {
                    unreachable!()
                };
                member.native.id = "other-owner".into();
            }
            _ => {
                let SourceObservation::Snapshot(header) = &mut foreign.observation else { unreachable!() };
                header.observed_until = "2020-01-01T12:00:02Z".into();
            }
        }
        assert!(ExportedRecord::new(&foreign, &capture, row(original.clone())).is_err(), "{change}");
    }
    let mut foreign = original.clone();
    let CollectionTarget::Database(database) = &mut foreign.context.target else { unreachable!() };
    database.engine = crate::Engine::Postgres;
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut foreign.context.partition else {
        unreachable!()
    };
    member.engine = crate::Engine::Postgres;
    assert!(foreign.context.has_qualified_local_owner());
    assert!(!foreign.is_valid(), "coherent local ownership is not SQL Server evidence");
    let mut unknown = original.clone();
    unknown.context.database_writable = None;
    assert!(!unknown.is_valid());
    unknown = original.clone();
    unknown.observed_at = "2020-01-01T12:00:02Z".into();
    assert!(!unknown.is_valid());
    unknown = original;
    unknown.reset = crate::ResetMarker::Generation("process-is-not-cdc-reset".into());
    assert!(!unknown.is_valid());
}
#[test]
fn original_position_preserves_duplicate_errors_without_cross_capture_event_identity() {
    let mut point = point();
    point.data = MssqlCdcData::Error(MssqlCdcError {
        session_id: 0,
        phase_number: None,
        entered_without_offset: Some("1900-01-01T00:00:00".into()),
        error_number: 2628,
        severity: 16,
        state: 1,
        start_lsn: Some("0".into()),
        begin_lsn: None,
        sequence_value: None,
    });
    let first = row(point.clone());
    point.position = 2;
    let second = row(point.clone());
    assert_ne!(first.native_identity(), second.native_identity());
    let mut source = description(&point);
    let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
    header.records = 2;
    header.sources.iter_mut().find(|coverage| coverage.source == point.source()).unwrap().records = 2;
    let a = ExportedRecord::new(
        &source,
        &CaptureIdentity { observation_id: "first-capture".into(), revision: None },
        first.clone(),
    )
    .unwrap();
    let b = ExportedRecord::new(
        &source,
        &CaptureIdentity { observation_id: "later-capture".into(), revision: None },
        first,
    )
    .unwrap();
    assert_ne!(a.record_id, b.record_id, "a reused native scan ID never becomes a unique error event feed");
    let MssqlCdcData::Error(error) = &mut point.data else { unreachable!() };
    error.severity = 0;
    assert!(!point.is_valid());
}
