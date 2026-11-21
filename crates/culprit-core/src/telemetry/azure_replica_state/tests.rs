use super::*;
use crate::SourceState;
fn source() -> CollectionDescription {
    let mut source = super::super::hyperscale_replicas::tests::source();
    let SourceObservation::MssqlReplication { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.sources[0] = StateSourceCoverage::observed(MSSQL_AZURE_REPLICA_STATE_SOURCE.into(), 1, false);
    source
}
fn point() -> MssqlAzureReplicaState {
    MssqlAzureReplicaState {
        database_id: 7,
        replica_id: "91487244-730d-4b83-8e49-d482241c7288".into(),
        is_local: true,
        database_writable: false,
        observed_at: "2020-01-01T00:00:00.5Z".into(),
        group_id: None,
        group_database_id: None,
        progress: MssqlReplicaDatabaseProgress {
            is_primary: Some(false),
            state: Some(0),
            is_suspended: Some(true),
            log_send_queue_kb: Some("9007199254740993".into()),
            redo_queue_kb: Some("0".into()),
            recovery_lsn: Some("1234567890123456789012345".into()),
            last_commit_without_offset: Some("2019-12-31T23:59:59.0030000".into()),
            ..Default::default()
        },
    }
}
fn row(point: MssqlAzureReplicaState) -> ReplicationObservation {
    ReplicationObservation {
        source: MSSQL_AZURE_REPLICA_STATE_SOURCE.into(),
        fact: ReplicationFact::AzureReplicaState(Box::new(point)),
    }
}
#[test]
fn native_local_identity_does_not_require_azure_group_ids_or_replace_null_progress_with_zero() {
    let source = source();
    let point = point();
    assert!(row(point.clone()).fits_source(&source));
    let read = ReplicationSnapshot::from_source(&source, vec![row(point.clone())]).unwrap();
    assert!(read.is_valid());
    assert_eq!(serde_json::from_value::<MssqlAzureReplicaState>(serde_json::to_value(&point).unwrap()).unwrap(), point);
    assert!(point.group_id.is_none() && point.group_database_id.is_none());
    assert_eq!(point.progress.redo_queue_kb.as_deref(), Some("0"));
    assert_eq!(point.progress.secondary_lag_seconds, None);
    assert_eq!(read.sources[0].state, SourceState::Partial);
    assert!(!read.sources[0].complete_scope);
    let mut nulls = point;
    nulls.progress = MssqlReplicaDatabaseProgress::default();
    assert!(ReplicationSnapshot::from_source(&source, vec![row(nulls)]).unwrap().is_valid());
}
#[test]
fn local_state_rejects_foreign_database_member_role_clock_nonlocal_rows_and_forged_catalog_mode() {
    for change in ["database", "member", "role", "clock", "local", "group", "mode", "counter", "position", "source"] {
        let mut point = point();
        match change {
            "database" => point.database_id = 8,
            "member" => point.replica_id = "11111111-1111-1111-1111-111111111111".into(),
            "role" => point.database_writable = true,
            "clock" => point.observed_at = "2020-01-01T00:00:02Z".into(),
            "local" => point.is_local = false,
            "group" => point.group_id = Some("display-name".into()),
            "mode" => point.progress.mode = Some(0),
            "counter" => point.progress.redo_queue_kb = Some("-1".into()),
            "position" => point.progress.recovery_lsn = Some("12345678901234567890123456".into()),
            _ => {}
        }
        let mut row = row(point);
        if change == "source" {
            row.source = "availability-group-state".into();
        }
        assert!(!row.fits_source(&source()), "{change}");
        assert!(ReplicationSnapshot::from_source(&source(), vec![row]).is_err(), "{change}");
    }
    let mut source = source();
    source.observation = SourceObservation::Snapshot(source.observation.snapshot().unwrap().clone());
    assert!(!row(point()).fits_source(&source));
}
#[test]
fn nullable_group_identity_and_native_progress_are_required_not_legacy_repair_or_quorum_substitutes() {
    for field in ["group_id", "group_database_id"] {
        let mut value = serde_json::to_value(point()).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<MssqlAzureReplicaState>(value).is_err(), "{field}");
    }
    let mut value = serde_json::to_value(point()).unwrap();
    value["progress"].as_object_mut().unwrap().remove("redo_queue_kb");
    assert!(serde_json::from_value::<MssqlAzureReplicaState>(value).is_err());
    let mut value = serde_json::to_value(point()).unwrap();
    value["quorum_commit_lsn"] = serde_json::json!("1");
    assert!(serde_json::from_value::<MssqlAzureReplicaState>(value).is_err());
}
#[test]
fn shared_progress_keeps_existing_ag_json_flat_and_ag_identity_requirements_unchanged() {
    let mut progress = point().progress;
    progress.mode = Some(1);
    progress.native_mode = Some("SYNCHRONOUS_COMMIT".into());
    let ag = AvailabilityReplicaSample {
        group_id: "11111111-1111-1111-1111-111111111111".into(),
        group_database_id: "22222222-2222-2222-2222-222222222222".into(),
        replica_id: point().replica_id,
        progress,
        ..Default::default()
    };
    assert!(ReplicationFact::AvailabilityGroup(Box::new(ag.clone())).is_valid());
    let value = serde_json::to_value(&ag).unwrap();
    assert!(value.get("progress").is_none());
    assert_eq!(value["redo_queue_kb"], "0");
    assert_eq!(value["mode"], 1);
    assert_eq!(serde_json::from_value::<AvailabilityReplicaSample>(value.clone()).unwrap(), ag);
    let mut invalid = value;
    invalid["group_id"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<AvailabilityReplicaSample>(invalid).is_err());
}
