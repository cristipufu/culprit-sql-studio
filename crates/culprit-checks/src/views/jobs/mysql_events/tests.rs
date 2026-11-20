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
            target: culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Mysql,
                "shop",
                "fixture.invalid",
                3306,
                None,
            )
            .into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: culprit_core::Engine::Mysql,
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

fn report(snapshot: MysqlEventsSnapshot) -> JobsReport {
    super::super::explain(&JobsSnapshot::MysqlEvents { snapshot: Box::new(snapshot), read_complete: true }).unwrap()
}

#[test]
fn disabled_event_and_off_configuration_explain_only_recorded_definition_and_last_start_limits() {
    let read = read();
    let report = report(read);
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].kind, JobEntryKind::State);
    assert!(report.entries[0].job_id.is_none());
    assert!(report.entries[0].explanation.contains(&t!("view.mysql_events.event.disabled")));
    assert!(report.limitations.contains(&t!("view.mysql_events.limit.outcome")));
    assert!(report.limitations.contains(&t!("view.mysql_events.limit.setting")));
    let last =
        report.entries[0].metrics.iter().find(|metric| metric.key == "view.mysql_events.field.last_started").unwrap();
    assert_eq!(last.text.as_deref(), Some(t!("view.mysql_events.unknown").as_str()));
}
#[test]
fn partial_visibility_and_missing_original_body_never_become_empty_success() {
    let mut read = read();
    read.metadata.database_event_visibility = false;
    read.sources[0].complete_scope = false;
    read.sources[0].state = SourceState::Partial;
    let partial = report(read.clone());
    assert!(partial.limitations.contains(&t!("view.mysql_events.limit.visibility")));
    assert_eq!(partial.sources[0].state, SourceState::Partial);
    read.observations.clear();
    let missing = super::explain(&read, false, 256).unwrap();
    assert_eq!(missing.native_rows, 1);
    assert_eq!(missing.observed_rows, 0);
    assert!(!missing.read_complete);
    assert!(missing.limitations.contains(&t!("view.jobs.limit.read")));
}
#[test]
fn permission_unsupported_and_known_empty_share_no_generic_failure_or_health_verdict() {
    for layout in [
        MysqlEventCatalogLayout::Available,
        MysqlEventCatalogLayout::Unsupported,
        MysqlEventCatalogLayout::PermissionDenied,
    ] {
        let mut read = read();
        read.observations.clear();
        read.metadata.layout = layout;
        read.sources[0] = if layout == MysqlEventCatalogLayout::Available {
            StateSourceCoverage::collected(MYSQL_SCHEDULED_EVENTS_SOURCE.into(), 0)
        } else {
            StateSourceCoverage::unavailable(
                MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
                if layout == MysqlEventCatalogLayout::PermissionDenied {
                    culprit_core::ErrorCode::PermissionDenied
                } else {
                    culprit_core::ErrorCode::Unsupported
                },
                None,
            )
        };
        let report = report(read);
        assert!(report.entries.is_empty());
        assert!(report.summary.contains("OFF"));
        assert!(report.limitations.contains(&t!("view.mysql_events.limit.outcome")));
    }
}
#[test]
fn current_configured_on_and_replica_disabled_or_unknown_state_never_claim_an_executing_worker() {
    for status in [
        None,
        Some(MysqlEventStatus::Unknown),
        Some(MysqlEventStatus::ReplicaSideDisabled),
        Some(MysqlEventStatus::SlavesideDisabled),
    ] {
        let mut read = read();
        read.metadata.scheduler_setting = Some(MysqlEventSchedulerSetting::On);
        read.sources[0].complete_scope = false;
        read.sources[0].state = SourceState::Partial;
        let OperationalFact::MysqlScheduledEvent(event) = &mut read.observations[0].fact else { unreachable!() };
        event.status = status;
        let report = report(read);
        assert!(report.summary.contains("ON"));
        assert_eq!(report.entries[0].kind, JobEntryKind::State);
        assert!(report.limitations.contains(&t!("view.mysql_events.limit.setting")));
    }
}
