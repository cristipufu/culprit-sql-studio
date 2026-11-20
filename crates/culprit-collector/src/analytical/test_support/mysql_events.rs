//! One native event corpus drives the SDK and both offline stores, without a second decoder.
use super::*;
use culprit_core::Engine;
#[derive(Clone, Copy)]
pub enum MysqlEventsCase {
    Disabled,
    Enabled,
    ReplicaDisabled,
    UnknownAndNull,
    Partial,
    Unavailable,
    Empty,
}
pub fn mysql_events_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    [
        MysqlEventsCase::Disabled,
        MysqlEventsCase::Enabled,
        MysqlEventsCase::ReplicaDisabled,
        MysqlEventsCase::UnknownAndNull,
        MysqlEventsCase::Partial,
        MysqlEventsCase::Unavailable,
        MysqlEventsCase::Empty,
    ]
    .into_iter()
    .map(mysql_events_evidence)
    .collect()
}
pub fn mysql_events_evidence(case: MysqlEventsCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let unknown = matches!(case, MysqlEventsCase::UnknownAndNull);
    let empty = matches!(case, MysqlEventsCase::Unavailable | MysqlEventsCase::Empty);
    let status = match case {
        MysqlEventsCase::Enabled => Some(MysqlEventStatus::Enabled),
        MysqlEventsCase::ReplicaDisabled => Some(MysqlEventStatus::ReplicaSideDisabled),
        MysqlEventsCase::UnknownAndNull => None,
        _ => Some(MysqlEventStatus::Disabled),
    };
    let point = MysqlScheduledEvent {
        observed_at: "2026-11-20T12:00:01Z".into(),
        schema: "shop".into(),
        name: "nightly_event".into(),
        status,
        event_type: (!unknown).then_some(MysqlEventType::Recurring),
        time_zone: (!unknown).then(|| "SYSTEM".into()),
        interval_value: (!unknown).then(|| "1".into()),
        interval_field: (!unknown).then(|| "DAY".into()),
        execute_at_without_offset: None,
        starts_without_offset: Some("2026-11-20T13:00:00".into()),
        ends_without_offset: None,
        last_started_without_offset: matches!(case, MysqlEventsCase::Enabled).then(|| "2026-11-19T13:00:00".into()),
        created_without_offset: Some("2026-11-20T10:00:00".into()),
        altered_without_offset: None,
    };
    let records = if empty {
        vec![]
    } else {
        vec![TelemetryRecord::Operational(OperationalObservation {
            source: MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
            key: point.key().unwrap(),
            fact: OperationalFact::MysqlScheduledEvent(Box::new(point)),
        })]
    };
    let mut coverage = if matches!(case, MysqlEventsCase::Unavailable) {
        StateSourceCoverage::unavailable(
            MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else {
        StateSourceCoverage::observed(
            MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
            records.len() as u32,
            !unknown && !matches!(case, MysqlEventsCase::Partial),
        )
    };
    if matches!(case, MysqlEventsCase::Partial) {
        coverage.stopped = Some(CollectionStop::RowLimit);
    }
    let member = SharedResourceIdentity {
        engine: Engine::Mysql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("mysql-server").qualified("server_uuid", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
        native: NativeIdentity::new("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
    };
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(Engine::Mysql, "shop", "fixture.invalid", 3306, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(member),
            epoch: Some("original-native-process".into()),
        },
        source: MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MysqlEvents {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Jobs,
                observed_from: "2026-11-20T12:00:00Z".into(),
                observed_until: "2026-11-20T12:00:02Z".into(),
                records: records.len() as u32,
                sources: vec![coverage],
            },
            metadata: Box::new(MysqlEventsMetadata {
                scheduler_setting: Some(if matches!(case, MysqlEventsCase::Enabled) {
                    MysqlEventSchedulerSetting::On
                } else {
                    MysqlEventSchedulerSetting::Off
                }),
                scheduler_observed_at: "2026-11-20T12:00:00Z".into(),
                database_event_visibility: !unknown,
                layout: if matches!(case, MysqlEventsCase::Unavailable) {
                    MysqlEventCatalogLayout::PermissionDenied
                } else {
                    MysqlEventCatalogLayout::Available
                },
                session_time_zone: Some("+00:00".into()),
                database_writable: true,
            }),
        },
    };
    let rows = records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = record else { unreachable!() };
            row.clone()
        })
        .collect();
    assert!(JobsSnapshot::from_source(&source, rows, true).unwrap().is_valid());
    (source, records)
}
