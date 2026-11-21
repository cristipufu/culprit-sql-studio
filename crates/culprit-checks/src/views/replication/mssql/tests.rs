use super::*;
use std::collections::BTreeMap;

const GROUP: &str = "11111111-2222-3333-4444-555555555555";
const REPLICA: &str = "22222222-2222-3333-4444-555555555555";

fn database() -> AvailabilityReplicaSample {
    AvailabilityReplicaSample {
        group_id: GROUP.into(),
        group_database_id: GROUP.into(),
        replica_id: REPLICA.into(),
        database_id: Some(7),
        database_name: Some("shop".into()),
        native_name: Some("native-secondary".into()),
        is_local: Some(false),
        progress: MssqlReplicaDatabaseProgress {
            is_primary: Some(false),
            state: Some(2),
            native_state: Some("SYNCHRONIZED".into()),
            synchronization_health: Some(2),
            mode: Some(1),
            native_mode: Some("SYNCHRONOUS_COMMIT".into()),
            is_suspended: Some(false),
            ..Default::default()
        },

        ..Default::default()
    }
}

fn replica() -> AvailabilityReplicaContext {
    AvailabilityReplicaContext {
        catalog_observed: true,
        state_observed: true,
        role: Some(2),
        native_role: Some("SECONDARY".into()),
        operational_state: None,
        native_operational_state: None,
        connected_state: Some(1),
        native_connected_state: Some("CONNECTED".into()),
        recovery_health: None,
        native_recovery_health: None,
        synchronization_health: Some(2),
        native_synchronization_health: Some("HEALTHY".into()),
        last_connect_error_number: None,
        last_connect_error_without_offset: None,
        failover_mode: Some(1),
        native_failover_mode: Some("MANUAL".into()),
        session_timeout_seconds: Some("10".into()),
        primary_role_allow_connections: Some(2),
        native_primary_role_allow_connections: Some("ALL".into()),
        secondary_role_allow_connections: Some(1),
        native_secondary_role_allow_connections: Some("READ_ONLY".into()),
        seeding_mode: None,
        native_seeding_mode: None,
        endpoint_url: None,
        read_only_routing_url: None,
        read_write_routing_url: None,
        backup_priority: Some(0),
        created_without_offset: None,
        modified_without_offset: None,
    }
}

fn group() -> AvailabilityGroupContext {
    AvailabilityGroupContext {
        catalog_observed: true,
        state_observed: true,
        name: Some("shop-group".into()),
        primary_replica: None,
        primary_recovery_health: None,
        native_primary_recovery_health: None,
        secondary_recovery_health: Some(1),
        native_secondary_recovery_health: Some("ONLINE".into()),
        synchronization_health: Some(0),
        native_synchronization_health: Some("NOT_HEALTHY".into()),
        failure_condition_level: Some(3),
        health_check_timeout_ms: Some("30000".into()),
        automated_backup_preference: Some(2),
        native_automated_backup_preference: Some("SECONDARY".into()),
        is_distributed: Some(false),
        is_basic: None,
        dtc_support: None,
        database_failover: None,
        cluster_type: None,
        native_cluster_type: None,
        required_synchronized_secondaries_to_commit: Some("0".into()),
        configuration_sequence: Some("9007199254740993".into()),
    }
}

fn explain_database(sample: AvailabilityReplicaSample) -> ReplicationEntry {
    let row = ReplicationObservation {
        source: "custom-native-ag".into(),
        fact: ReplicationFact::AvailabilityGroup(Box::new(sample)),
    };
    assert!(row.is_valid());
    let ReplicationFact::AvailabilityGroup(sample) = &row.fact else { panic!() };
    let result = availability(&row, sample);
    assert_eq!(result.conditions, availability_conditions(sample));
    assert_eq!(result.reference.source, row.source);
    assert_eq!(result.reference.observation_key, row.fact.key());
    result
}

fn values(entry: &ReplicationEntry) -> BTreeMap<&str, Option<&str>> {
    let fields = entry.fields.iter().chain(entry.groups.iter().flat_map(|group| &group.fields)).collect::<Vec<_>>();
    let values = fields.iter().map(|field| (field.key.as_str(), field.value.as_deref())).collect::<BTreeMap<_, _>>();
    assert_eq!(values.len(), fields.len(), "scoped native fields must not overwrite another scope");
    values
}

#[test]
fn exact_native_positions_queues_and_unzoned_times_are_preserved_without_an_eta_or_query_owner() {
    let mut sample = database();
    sample.progress.recovery_lsn = Some("9999999999999999999999999".into());
    sample.progress.last_redone_lsn = Some("9007199254740993".into());
    sample.progress.last_commit_lsn = Some("9007199254740992".into());
    sample.progress.last_sent_without_offset = Some("2020-01-01T01:00:00.0030000".into());
    sample.progress.log_send_queue_kb = Some("9007199254740993".into());
    sample.progress.redo_queue_kb = Some("0".into());
    sample.progress.redo_rate_kb_per_second = Some("123456789".into());
    let result = explain_database(sample);
    let fields = values(&result);
    assert_eq!(fields["recovery_lsn"], Some("9999999999999999999999999"));
    assert_eq!(fields["last_redone_lsn"], Some("9007199254740993"));
    assert_eq!(fields["log_send_queue_kb"], Some("9007199254740993"));
    assert_eq!(fields["redo_queue_kb"], Some("0"));
    assert_eq!(fields["last_sent_without_offset"], Some("2020-01-01T01:00:00.0030000"));
    assert_eq!(fields["last_received_without_offset"], None);
    assert_eq!(fields["log_send_rate_kb_per_second"], None);
    assert!(result.session.is_none() && result.unresolved_session_id.is_none());
    assert!(result.conditions.is_empty());
    assert_eq!(result.scope, MeasurementScope::Database);
    let positions =
        result.groups.iter().find(|group| group.title == t!("view.replication.mssql.group.positions")).unwrap();
    assert_eq!(positions.fields[0].label, t!("view.replication.mssql.field.recovery_position"));
    assert_eq!(positions.fields[5].label, t!("view.replication.mssql.field.redone_lsn"));
    assert!(result.groups.iter().any(|group| group.title == t!("view.replication.mssql.group.movement")));
    assert!(fields.keys().all(|key| !key.contains("eta") && !key.contains("estimated_drain")));
}

#[test]
fn a_suspended_native_zero_lag_point_is_not_recovery_and_reason_only_explains_actual_suspension() {
    let mut sample = database();
    sample.progress.suspend_reason = Some(0);
    sample.progress.native_suspend_reason = Some("SUSPEND_FROM_USER".into());
    sample.progress.secondary_lag_seconds = Some("0".into());
    for suspended in [Some(true), Some(false), None] {
        sample.progress.is_suspended = suspended;
        let result = explain_database(sample.clone());
        assert_eq!(result.conditions.contains(&ReplicationCondition::MovementSuspended), suspended == Some(true));
        assert_eq!(values(&result)["suspend_reason_code"], if suspended == Some(true) { Some("0") } else { None });
        assert_eq!(values(&result)["secondary_lag_seconds"], Some("0"));
        assert_eq!(
            result.explanation.contains(&t!("view.replication.mssql.availability.suspended")),
            suspended == Some(true)
        );
        assert_eq!(sample.progress.suspend_reason, Some(0), "the original raw context is untouched");
    }
}

#[test]
fn current_disconnection_and_historical_connection_error_are_independent_native_facts() {
    let mut sample = database();
    sample.replica = Some(Box::new(replica()));
    let context = sample.replica.as_mut().unwrap();
    context.last_connect_error_number = Some(35206);
    context.last_connect_error_without_offset = Some("2020-01-01T00:00:00.0030000".into());
    let result = explain_database(sample.clone());
    assert!(result.conditions.is_empty(), "an old error is not a present disconnection");
    assert!(result.explanation.contains(&t!("view.replication.mssql.availability.historical_error")));
    assert!(!result.explanation.contains(&t!("view.replication.mssql.availability.disconnected")));
    assert_eq!(values(&result)["replica.last_connect_error_number"], Some("35206"));
    assert_eq!(values(&result)["replica.operational_state_code"], None);
    sample.replica.as_mut().unwrap().connected_state = Some(0);
    sample.replica.as_mut().unwrap().native_connected_state = Some("DISCONNECTED".into());
    let result = explain_database(sample);
    assert_eq!(result.conditions, vec![ReplicationCondition::Disconnected]);
    assert!(result.explanation.contains(&t!("view.replication.mssql.availability.disconnected")));
}

#[test]
fn cross_database_group_health_and_missing_context_cannot_become_this_database_condition() {
    let mut sample = database();
    sample.group = Some(Box::new(group()));
    let result = explain_database(sample);
    let fields = values(&result);
    assert!(result.conditions.is_empty(), "group rollup can include a different database");
    assert_eq!(fields["synchronization_health_code"], Some("2"));
    assert_eq!(fields["group.synchronization_health_code"], Some("0"));
    assert_eq!(fields["group.catalog_observed"], Some("1"));
    assert_eq!(fields["group.primary_recovery_health_code"], None);
    assert_eq!(fields["group.is_distributed"], Some("0"));
    assert_eq!(fields["group.configuration_sequence"], Some("9007199254740993"));
    assert_eq!(fields["replica.state_observed"], None);
    let result = explain_database(database());
    assert_eq!(values(&result)["group.catalog_observed"], None);
    assert_eq!(values(&result)["group.state_observed"], None);
}

#[test]
fn healthy_unknown_and_not_synchronizing_codes_are_classified_without_native_text_rules() {
    let mut sample = database();
    for state in [None, Some(2), Some(93)] {
        sample.progress.state = state;
        sample.progress.native_state = Some("NOT SYNCHRONIZING".into());
        assert!(
            explain_database(sample.clone()).conditions.is_empty(),
            "code {state:?} has no supported fault inference"
        );
    }
    sample.progress.state = Some(0);
    sample.progress.native_state = Some("future-native-display-name".into());
    assert_eq!(explain_database(sample).conditions, vec![ReplicationCondition::NotSynchronizing]);
}

fn explain_geo(sample: GeoReplicationSample) -> ReplicationEntry {
    let row =
        ReplicationObservation { source: "custom-native-geo".into(), fact: ReplicationFact::GeoLink(Box::new(sample)) };
    assert!(row.is_valid());
    let ReplicationFact::GeoLink(sample) = &row.fact else { panic!() };
    let result = geo(&row, sample);
    assert_eq!(result.conditions, geo_conditions(sample));
    assert_eq!(result.reference.source, row.source);
    assert_eq!(result.reference.observation_key, row.fact.key());
    result
}

#[test]
fn geo_primary_secondary_and_unknown_roles_keep_separate_clock_availability() {
    let sample = GeoReplicationSample {
        link_id: GROUP.into(),
        role: Some(0),
        state: Some(2),
        lag_seconds: Some("0".into()),
        last_replication_at: Some("2020-01-01T00:00:00Z".into()),
        last_commit_at: Some("2020-01-01T00:00:01.123456700Z".into()),
        secondary_allow_connections: Some(0),
        ..Default::default()
    };
    let result = explain_geo(sample.clone());
    assert_eq!(values(&result)["lag_seconds"], Some("0"));
    assert_eq!(values(&result)["secondary_allow_connections_code"], Some("0"));
    assert!(result.conditions.is_empty());
    for role in [Some(1), Some(93), None] {
        let mut sample = sample.clone();
        sample.role = role;
        sample.lag_seconds = None;
        sample.last_replication_at = None;
        let result = explain_geo(sample);
        assert_eq!(values(&result)["lag_seconds"], None);
        assert_eq!(values(&result)["last_replication_at"], None);
        assert_eq!(values(&result)["last_commit_at"], Some("2020-01-01T00:00:01.123456700Z"));
        assert!(result.conditions.is_empty());
        assert_eq!(
            result.explanation,
            if role == Some(1) {
                t!("view.replication.mssql.geo.secondary")
            } else {
                t!("view.replication.mssql.geo.unknown")
            }
        );
    }
}

#[test]
fn geo_seeding_and_unknown_states_are_not_failed_replication_and_suspension_does_not_identify_a_cause() {
    for state in [Some(1), Some(2), Some(93), None, Some(4)] {
        let result =
            explain_geo(GeoReplicationSample { link_id: GROUP.into(), role: Some(1), state, ..Default::default() });
        assert_eq!(result.conditions.contains(&ReplicationCondition::MovementSuspended), state == Some(4));
        assert_eq!(result.explanation.contains(&t!("view.replication.mssql.geo.seeding")), state == Some(1));
        assert!(result.session.is_none() && result.unresolved_session_id.is_none());
    }
}
