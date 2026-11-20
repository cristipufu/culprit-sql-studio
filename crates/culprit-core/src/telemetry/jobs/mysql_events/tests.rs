use super::*;
fn read() -> MysqlEventsSnapshot {
    let point = MysqlScheduledEvent {
        observed_at: "2026-11-20T12:00:01Z".into(),
        schema: "shop".into(),
        name: "nightly".into(),
        status: Some(MysqlEventStatus::Disabled),
        event_type: Some(MysqlEventType::Recurring),
        time_zone: Some("SYSTEM".into()),
        interval_value: Some("1".into()),
        interval_field: Some("DAY".into()),
        execute_at_without_offset: None,
        starts_without_offset: None,
        ends_without_offset: None,
        last_started_without_offset: None,
        created_without_offset: Some("2026-11-20T10:00:00".into()),
        altered_without_offset: None,
    };
    let row = OperationalObservation {
        source: MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MysqlScheduledEvent(Box::new(point)),
    };
    MysqlEventsSnapshot {
        context: NativeReadContext {
            target: crate::DatabaseIdentity::server(crate::Engine::Mysql, "shop", "fixture.invalid", 3306, None).into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: crate::Engine::Mysql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("mysql-server")
                        .qualified("server_uuid", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
                    native: NativeIdentity::new("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
                }),
                epoch: Some("native-process".into()),
            },
            observed_from: "2026-11-20T12:00:00Z".into(),
            observed_until: "2026-11-20T12:00:02Z".into(),
            database_writable: Some(true),
        },
        metadata: MysqlEventsMetadata {
            scheduler_setting: Some(MysqlEventSchedulerSetting::Off),
            scheduler_observed_at: "2026-11-20T12:00:00Z".into(),
            database_event_visibility: true,
            layout: MysqlEventCatalogLayout::Available,
            session_time_zone: Some("+00:00".into()),
            database_writable: true,
        },
        sources: vec![StateSourceCoverage::collected(MYSQL_SCHEDULED_EVENTS_SOURCE.into(), 1)],
        observations: vec![row],
    }
}
fn source(read: &MysqlEventsSnapshot) -> CollectionDescription {
    CollectionDescription {
        target: read.context.target.clone(),
        partition: read.context.partition.clone(),
        source: MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MysqlEvents {
            metadata: Box::new(read.metadata.clone()),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Jobs,
                observed_from: read.context.observed_from.clone(),
                observed_until: read.context.observed_until.clone(),
                records: read.sources.iter().map(|coverage| coverage.records).sum(),
                sources: read.sources.clone(),
            },
        },
    }
}

#[test]
fn original_native_owner_context_and_null_last_start_roundtrip_without_a_result_or_event_incarnation() {
    let read = read();
    assert!(read.is_valid_read(true));
    let source = source(&read);
    let recovered = MysqlEventsSnapshot::from_source(&source, read.observations.clone()).unwrap();
    assert_eq!(read, recovered);
    let encoded = serde_json::to_string(&recovered).unwrap();
    let decoded: MysqlEventsSnapshot = serde_json::from_str(&encoded).unwrap();
    assert!(decoded.is_valid_read(true));
    assert!(!encoded.contains("success"));
    assert!(!encoded.contains("run_id"));
    assert_eq!(source.target.database().unwrap().incarnation, None);
}
#[test]
fn raw_export_refuses_foreign_vendor_scope_clock_schema_and_source_even_with_coherent_coverage() {
    let read = read();
    let original = source(&read);
    for wrong in ["vendor", "member", "epoch", "dataset", "source", "schema", "clock"] {
        let mut source = original.clone();
        let mut row = read.observations[0].clone();
        match wrong {
            "vendor" => {
                let CollectionTarget::Database(database) = &mut source.target else { unreachable!() };
                database.engine = crate::Engine::Postgres;
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    unreachable!()
                };
                member.engine = crate::Engine::Postgres;
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    unreachable!()
                };
                member.kind = SharedResourceKind::Node;
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { unreachable!() };
                *epoch = None;
            }
            "source" => {
                source.source = "foreign".into();
                row.source = "foreign".into();
                let SourceObservation::MysqlEvents { snapshot, .. } = &mut source.observation else { unreachable!() };
                snapshot.sources[0].source = "foreign".into();
            }
            "dataset" => {
                let SourceObservation::MysqlEvents { snapshot, .. } = &mut source.observation else { unreachable!() };
                snapshot.dataset = SnapshotDataset::Operational;
            }
            "schema" | "clock" => {
                let OperationalFact::MysqlScheduledEvent(point) = &mut row.fact else { unreachable!() };
                if wrong == "schema" {
                    point.schema = "another".into()
                } else {
                    point.observed_at = "2026-11-20T13:00:00Z".into()
                };
                row.key = point.key().unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            ExportedRecord::new(
                &source,
                &CaptureIdentity { observation_id: "original".into(), revision: None },
                TelemetryRecord::Operational(row)
            )
            .is_err(),
            "{wrong}"
        );
    }
}
#[test]
fn body_gaps_and_duplicate_event_names_cannot_change_native_population_or_complete_history() {
    let mut read = read();
    read.observations.clear();
    assert!(read.is_valid_read(false));
    assert!(!read.is_valid_read(true));
    let mut read = super::tests::read();
    read.observations.push(read.observations[0].clone());
    read.sources[0].records = 2;
    assert!(!read.is_valid_read(false));
}
#[test]
fn unproven_visibility_unknown_native_states_and_denial_remain_distinct_from_known_empty_catalog() {
    let mut read = read();
    read.metadata.database_event_visibility = false;
    assert!(!read.is_valid_read(true));
    read.sources[0] = StateSourceCoverage::observed(MYSQL_SCHEDULED_EVENTS_SOURCE.into(), 1, false);
    assert!(read.is_valid_read(true));
    read.observations.clear();
    read.sources[0] = StateSourceCoverage::observed(MYSQL_SCHEDULED_EVENTS_SOURCE.into(), 0, false);
    assert!(read.is_valid_read(true));
    read.metadata.layout = MysqlEventCatalogLayout::PermissionDenied;
    assert!(!read.is_valid_read(true));
    read.sources[0] = StateSourceCoverage::unavailable(
        MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
        crate::ErrorCode::PermissionDenied,
        None,
    );
    assert!(read.is_valid_read(true));
}
#[test]
fn local_catalog_timestamps_never_accept_utc_offsets_and_native_nulls_are_not_zero_dates() {
    let mut read = read();
    let OperationalFact::MysqlScheduledEvent(event) = &mut read.observations[0].fact else { unreachable!() };
    assert!(event.last_started_without_offset.is_none());
    event.last_started_without_offset = Some("2026-11-20T12:00:00Z".into());
    assert!(!event.is_valid());
    event.last_started_without_offset = Some("0000-00-00T00:00:00".into());
    assert!(!event.is_valid());
}
