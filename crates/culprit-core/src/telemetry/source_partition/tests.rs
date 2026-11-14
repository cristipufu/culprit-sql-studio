use super::*;
use crate::{Engine, MeasurementScope, SourceState};
use serde_json::json;

fn member(name: &str) -> SharedResourceIdentity {
    SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Node,
        namespace: NativeIdentity::new("native-logical-database"),
        native: NativeIdentity::new(name),
    }
}

fn source(partition: SourcePartition) -> CollectionDescription {
    let epoch = match &partition {
        SourcePartition::Target => Some("native-start".into()),
        SourcePartition::InstanceLocal { epoch, .. } => epoch.clone(),
    };
    let complete = partition.is_qualified();
    CollectionDescription {
        partition,
        target: DatabaseIdentity::server(Engine::Mssql, "db", "logical.invalid", 1433, None).into(),
        source: "native-activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            database_writable: Some(true),
            epoch,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:00Z".into(),
            at: "2020-01-01T12:00:00Z".into(),
            scope: MeasurementScope::Database,
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::observed("native-activity".into(), 1, complete),
        }),
    }
}

fn local(name: &str, epoch: &str) -> SourcePartition {
    SourcePartition::InstanceLocal { member: Some(member(name)), epoch: Some(epoch.into()) }
}

fn row() -> ActivityObservation {
    serde_json::from_value(json!({
        "key":{"session":{"id":"61","generation":"login-1"},"request":{"id":"0","generation":"request-1"}},
        "blocking_status":"known", "waiting_on_lock":null, "idle":false, "scope":"request-database",
        "state":"running", "database":"db", "open_transactions":null,
        "session_counters":{},"request_counters":{},
        "statement":{"state":"unavailable","reason":"not-collected"}
    }))
    .unwrap()
}

fn capture(sequence: &str) -> CaptureIdentity {
    CaptureIdentity {
        observation_id: "same-observation-token".into(),
        revision: Some(CaptureRevision {
            outbox_id: "outbox".into(),
            owner_epoch: "1".into(),
            sequence: sequence.into(),
        }),
    }
}

#[test]
fn actual_members_partition_local_heads_ranges_and_definitions_without_changing_the_database() {
    let left = source(local("member-a", "native-start"));
    let right = source(local("member-b", "native-start"));
    assert_eq!(left.target.telemetry_id().unwrap(), right.target.telemetry_id().unwrap());
    assert_ne!(left.logical_source_id().unwrap(), right.logical_source_id().unwrap());
    assert_ne!(left.captured_range().unwrap().scope_id, right.captured_range().unwrap().scope_id);
    let query = NativeQueryIdentity { query_id: "7".into(), qualifiers: Default::default() };
    assert_ne!(query.definition_id(&left).unwrap(), query.definition_id(&right).unwrap());
    assert!(!DiagnosticScope::from_source(&left).unwrap().same_member(&DiagnosticScope::from_source(&right).unwrap()));
}

#[test]
fn role_is_observed_state_and_native_restart_changes_scope_without_creating_another_member() {
    let original = source(local("member-a", "native-start"));
    let mut role = original.clone();
    let SourceObservation::Activity(frame) = &mut role.observation else { panic!() };
    frame.database_writable = Some(false);
    assert_ne!(original, role);
    assert_eq!(original.logical_source_id().unwrap(), role.logical_source_id().unwrap());
    assert_eq!(original.captured_range().unwrap().scope_id, role.captured_range().unwrap().scope_id);
    let restarted = source(local("member-a", "new-native-start"));
    assert_eq!(original.logical_source_id().unwrap(), restarted.logical_source_id().unwrap());
    assert_ne!(original.captured_range().unwrap().scope_id, restarted.captured_range().unwrap().scope_id);
    let before = DiagnosticScope::from_source(&original).unwrap();
    let after = DiagnosticScope::from_source(&restarted).unwrap();
    assert!(before.same_member(&after));
    assert_ne!(before.key().unwrap(), after.key().unwrap());
}

#[test]
fn target_history_has_one_database_namespace_independent_of_native_replica_cohorts() {
    let mut target = source(SourcePartition::Target);
    target.observation = SourceObservation::QueryHistory(QueryHistoryDescription {
        population_catalog: None,
        waits: None,
        interval: TelemetryInterval {
            native_id: "7".into(),
            from: "2020-01-01T11:00:00Z".into(),
            until: "2020-01-01T12:00:00Z".into(),
        },
        dimensions: [("replica_group_id".into(), vec!["1".into(), "2".into()])].into(),
        native_properties: [("reader_member".into(), "member-a".into())].into(),
    });
    let mut another = target.clone();
    let SourceObservation::QueryHistory(history) = &mut another.observation else { panic!() };
    history.native_properties.insert("reader_member".into(), "member-b".into());
    assert_ne!(target, another);
    assert_eq!(target.logical_source_id().unwrap(), another.logical_source_id().unwrap());
    assert_eq!(target.captured_range().unwrap().scope_id, another.captured_range().unwrap().scope_id);
    let query = NativeQueryIdentity { query_id: "7".into(), qualifiers: Default::default() };
    assert_eq!(query.definition_id(&target).unwrap(), query.definition_id(&another).unwrap());
}

#[test]
fn unknown_local_authority_keeps_partial_points_capture_local_and_denies_stable_definition_or_history_identity() {
    for partition in [
        SourcePartition::InstanceLocal { member: None, epoch: None },
        SourcePartition::InstanceLocal { member: Some(member("member-a")), epoch: None },
        SourcePartition::InstanceLocal { member: None, epoch: Some("native-start".into()) },
    ] {
        let native = source(partition);
        assert!(native.partition_is_consistent());
        assert!(!native.is_partition_qualified());
        assert_eq!(
            native.partition.same_member(&native.partition),
            matches!(&native.partition, SourcePartition::InstanceLocal { member: Some(_), .. })
        );
        let first = ExportedRecord::new(&native, &capture("1"), TelemetryRecord::from(row())).unwrap();
        let later = ExportedRecord::new(&native, &capture("2"), TelemetryRecord::from(row())).unwrap();
        assert_ne!(first.record_id, later.record_id, "collector revisions isolate otherwise unidentified raw points");
        let query = NativeQueryIdentity { query_id: "7".into(), qualifiers: Default::default() };
        assert!(query.definition_id(&native).is_err());
        let plan = NativePlanIdentity { plan_id: "8".into(), query, qualifiers: Default::default() };
        assert!(plan.definition_id(&native).is_err());
        let mut forged = native;
        let SourceObservation::Activity(frame) = &mut forged.observation else { panic!() };
        frame.coverage.state = SourceState::Collected;
        frame.coverage.complete_scope = true;
        assert!(!forged.partition_is_consistent());
        assert!(ExportedRecord::new(&forged, &capture("1"), TelemetryRecord::from(row())).is_err());
    }
}

#[test]
fn wrong_engine_member_and_contradictory_native_epochs_cannot_gain_partition_authority() {
    let mut wrong = source(local("member-a", "native-start"));
    let SourcePartition::InstanceLocal { member, .. } = &mut wrong.partition else { panic!() };
    member.as_mut().unwrap().engine = Engine::Postgres;
    assert!(!wrong.partition_is_consistent());
    assert!(wrong.captured_range().is_err());
    let mut wrong = source(local("member-a", "native-start"));
    let SourceObservation::Activity(frame) = &mut wrong.observation else { panic!() };
    frame.epoch = Some("different-native-start".into());
    assert!(!wrong.partition_is_consistent());
    assert!(DiagnosticScope::from_source(&wrong).is_err());
}
