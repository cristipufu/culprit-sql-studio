use super::*;
fn point() -> MssqlChangeFeedObservation {
    MssqlChangeFeedObservation {
        context: super::super::native_read_context::tests::read_context(),
        database_id: 5,
        observed_at: "2020-01-01T12:00:00.500Z".into(),
        position: 1,
        reset: crate::ResetMarker::Unknown,
        data: MssqlChangeFeedData::Availability(MssqlChangeFeedAvailability {
            native_major: 16,
            native_edition: 3,
            has_database_state: Some(true),
            has_database_performance_state: Some(false),
            scan_layout_available: true,
            error_layout_available: true,
        }),
    }
}
fn source(point: &MssqlChangeFeedObservation) -> CollectionDescription {
    CollectionDescription {
        target: point.context.target.clone(),
        partition: point.context.partition.clone(),
        source: "mssql-change-feed".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::ChangeFeed,
            observed_from: point.context.observed_from.clone(),
            observed_until: point.context.observed_until.clone(),
            records: 1,
            sources: MssqlChangeCaptureFamily::ChangeFeed
                .sources()
                .into_iter()
                .map(|name| {
                    if name == point.source() {
                        StateSourceCoverage::collected(name.into(), 1)
                    } else {
                        StateSourceCoverage::collected(name.into(), 0)
                    }
                })
                .collect(),
        }),
    }
}
fn row(point: MssqlChangeFeedObservation) -> TelemetryRecord {
    TelemetryRecord::Operational(OperationalObservation {
        source: point.source().into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MssqlChangeFeed(Box::new(point)),
    })
}
#[test]
fn either_documented_permission_preserves_false_null_and_unknown_separately() {
    for (state, performance, expected) in [
        (Some(true), None, Some(true)),
        (None, Some(true), Some(true)),
        (Some(false), Some(false), Some(false)),
        (Some(false), None, None),
        (None, None, None),
    ] {
        let mut p = point();
        let MssqlChangeFeedData::Availability(a) = &mut p.data else { unreachable!() };
        a.has_database_state = state;
        a.has_database_performance_state = performance;
        assert_eq!(a.has_permission(), expected);
        assert!(p.is_valid());
    }
}
#[test]
fn original_change_feed_cannot_borrow_cdc_dataset_other_vendor_member_reset_or_native_clock() {
    let original = point();
    let valid = source(&original);
    let capture = CaptureIdentity { observation_id: "change-feed-original".into(), revision: None };
    assert!(ExportedRecord::new(&valid, &capture, row(original.clone())).is_ok());
    for mutation in ["cdc", "vendor", "member", "bounds", "reset", "point-clock", "position"] {
        let mut s = valid.clone();
        let mut p = original.clone();
        match mutation {
            "cdc" => {
                let SourceObservation::Snapshot(h) = &mut s.observation else { unreachable!() };
                h.dataset = SnapshotDataset::Cdc;
            }
            "vendor" => {
                let CollectionTarget::Database(db) = &mut s.target else { unreachable!() };
                db.engine = crate::Engine::Postgres;
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut s.partition else {
                    unreachable!()
                };
                member.engine = crate::Engine::Postgres;
                p.context.target = s.target.clone();
                p.context.partition = s.partition.clone();
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut s.partition else {
                    unreachable!()
                };
                member.native.id = "foreign".into();
            }
            "bounds" => {
                let SourceObservation::Snapshot(h) = &mut s.observation else { unreachable!() };
                h.observed_until = "2020-01-01T12:00:02Z".into();
            }
            "reset" => p.reset = crate::ResetMarker::Generation("process-is-not-change-feed-reset".into()),
            "point-clock" => p.observed_at = "2020-01-01T12:00:02Z".into(),
            _ => p.position = 2,
        }
        let bad = TelemetryRecord::Operational(OperationalObservation {
            source: p.source().into(),
            key: format!("change-feed:{}:{}", p.source(), p.position),
            fact: OperationalFact::MssqlChangeFeed(Box::new(p)),
        });
        assert!(ExportedRecord::new(&s, &capture, bad).is_err(), "{mutation}");
    }
}
#[test]
fn native_task_table_group_and_snapshot_nulls_are_context_not_current_ownership_or_events() {
    let mut p = point();
    p.data = MssqlChangeFeedData::Error(Box::new(MssqlChangeFeedError {
        session_id: 0,
        source_task: Some(255),
        table_id: None,
        table_group_id: None,
        capture_phase_number: None,
        entered_without_offset: Some("2099-01-01T01:00:00".into()),
        error_number: 2628,
        severity: 16,
        state: 0,
        command_id: None,
        batch_start_lsn: Some("0".into()),
        batch_end_lsn: Some("0".into()),
        transaction_begin_lsn: None,
        transaction_commit_lsn: None,
        sequence_value: None,
    }));
    assert!(p.is_valid());
    let restored: MssqlChangeFeedObservation = serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
    assert_eq!(restored, p);
    let mut value = serde_json::to_value(&p).unwrap();
    value["data"]["value"]["error_message"] = serde_json::json!("PRIVATE-CANARY");
    assert!(serde_json::from_value::<MssqlChangeFeedObservation>(value).is_err());
    let MssqlChangeFeedData::Error(e) = &mut p.data else { unreachable!() };
    e.table_group_id = Some("not-a-native-guid".into());
    assert!(!p.is_valid());
}

#[test]
fn nullable_native_fields_are_required_on_wire_and_explicit_null_never_becomes_zero_or_false() {
    let p = point();
    let mut value = serde_json::to_value(&p).unwrap();
    value["data"]["value"]["has_database_state"] = serde_json::Value::Null;
    let parsed: MssqlChangeFeedObservation = serde_json::from_value(value.clone()).unwrap();
    let MssqlChangeFeedData::Availability(a) = parsed.data else { unreachable!() };
    assert_eq!(a.has_database_state, None);
    assert_eq!(a.has_permission(), None);
    value["data"]["value"].as_object_mut().unwrap().remove("has_database_state");
    assert!(serde_json::from_value::<MssqlChangeFeedObservation>(value).is_err());
}
