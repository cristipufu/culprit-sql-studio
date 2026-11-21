use super::*;
use crate::{DatabaseIdentity, Engine, SourceState};
const LOCAL: &str = "91487244-730d-4b83-8e49-d482241c7288";
const REMOTE: &str = "11111111-1111-1111-1111-111111111111";
fn source() -> CollectionDescription {
    let mut database = DatabaseIdentity::server(Engine::Mssql, "app", "owned.invalid", 1433, None);
    database.incarnation = Some("logical-original".into());
    CollectionDescription {
        target: database.into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Node,
                namespace: NativeIdentity::new("logical-original").qualified("kind", "azure-sql-logical-database"),
                native: NativeIdentity::new(LOCAL).qualified("kind", "azure-sql-hyperscale-replica"),
            }),
            epoch: Some("native-original-process".into()),
        },
        source: "replication-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MssqlReplication {
            scope: Box::new(MssqlReplicationScope { database_id: 7, replica_id: LOCAL.into() }),
            database_writable: false,
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Replication,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(MSSQL_HYPERSCALE_REPLICAS_SOURCE.into(), 1, false)],
            },
        },
    }
}
fn point() -> MssqlHyperscaleReplica {
    MssqlHyperscaleReplica {
        selected_database_id: 7,
        observer_replica_id: LOCAL.into(),
        database_writable: false,
        observed_at: "2020-01-01T00:00:00.5Z".into(),
        replica_id: REMOTE.into(),
        replica_server_name: Some("logical-server".into()),
        replica_database_name: Some("app".into()),
        is_local: Some(false),
        replica_role: Some(1),
        native_role: Some("HA SECONDARY".into()),
        local_database_id: Some(70),
        parent_replica_id: Some(LOCAL.into()),
    }
}
fn row(point: MssqlHyperscaleReplica) -> ReplicationObservation {
    ReplicationObservation {
        source: MSSQL_HYPERSCALE_REPLICAS_SOURCE.into(),
        fact: ReplicationFact::HyperscaleReplica(Box::new(point)),
    }
}
#[test]
fn original_replication_header_admits_only_the_matching_bounded_snapshot_request() {
    let original = source();
    let mut request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Replication },
        budget: CollectionBudget { page_rows: 1, max_snapshot_rows: 1, ..Default::default() },
    };
    assert!(original.matches_request(&request));
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::Settings };
    assert!(!original.matches_request(&request));
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::Replication };
    let mut foreign = original.clone();
    let SourceObservation::MssqlReplication { scope, .. } = &mut foreign.observation else { panic!() };
    scope.replica_id = REMOTE.into();
    assert!(!foreign.matches_request(&request));
    request.budget.max_snapshot_rows = 0;
    assert!(!original.matches_request(&request));
}
#[test]
fn topology_preserves_remote_database_ids_nullable_edges_unknown_codes_and_original_header() {
    let original = source();
    for variant in ["remote", "local", "local-null-id", "null", "future", "zero"] {
        let mut point = point();
        match variant {
            "local" | "local-null-id" => {
                point.replica_id = LOCAL.into();
                point.parent_replica_id = None;
                point.is_local = Some(true);
                point.local_database_id = (variant != "local-null-id").then_some(7);
            }
            "null" => {
                point.is_local = None;
                point.replica_role = None;
                point.native_role = None;
                point.local_database_id = None;
                point.parent_replica_id = None;
            }
            "future" => {
                point.replica_role = Some(255);
                point.parent_replica_id = None;
            }
            "zero" => point.local_database_id = Some(0),
            _ => {}
        }
        let row = row(point.clone());
        assert!(row.fits_source(&original), "{variant}");
        let read = ReplicationSnapshot::from_source(&original, vec![row.clone()]).unwrap();
        assert!(read.is_valid());
        assert!(!read.sources[0].complete_scope);
        assert_eq!(read.mssql_scope.as_deref(), mssql_replication_scope(&original).unwrap());
        assert_eq!(serde_json::from_value::<ReplicationSnapshot>(serde_json::to_value(&read).unwrap()).unwrap(), read);
        let capture = CaptureIdentity { observation_id: "original-capture".into(), revision: None };
        assert_eq!(
            ExportedRecord::new(&original, &capture, TelemetryRecord::Replication(row.clone())).unwrap().record,
            TelemetryRecord::Replication(row)
        );
    }
}
#[test]
fn topology_body_cannot_lend_native_database_replica_role_or_clock_to_the_original() {
    for change in ["database", "observer", "role", "clock", "local", "parent", "wrong-parent", "guid", "name", "source"]
    {
        let original = source();
        let mut point = point();
        match change {
            "database" => point.selected_database_id = 8,
            "observer" => point.observer_replica_id = REMOTE.into(),
            "role" => point.database_writable = true,
            "clock" => point.observed_at = "2020-01-01T00:00:02Z".into(),
            "local" => point.is_local = Some(true),
            "parent" => point.parent_replica_id = Some(point.replica_id.clone()),
            "wrong-parent" => point.replica_role = Some(3),
            "guid" => point.replica_id = "logical-name".into(),
            "name" => point.replica_server_name = Some("line\nbreak".into()),
            _ => {}
        }
        let mut row = row(point);
        if change == "source" {
            row.source = "availability-group-state".into();
        }
        assert!(!row.fits_source(&original), "{change}");
        assert!(ReplicationSnapshot::from_source(&original, vec![row]).is_err(), "{change}");
    }
}
#[test]
fn topology_original_requires_qualified_native_header_not_a_generic_row_derived_or_display_scope() {
    for change in ["generic", "member", "namespace", "epoch", "engine", "database", "scope", "dataset", "complete"] {
        let mut original = source();
        match change {
            "generic" => {
                original.observation = SourceObservation::Snapshot(original.observation.snapshot().unwrap().clone())
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut original.partition else {
                    panic!()
                };
                member.native.id = REMOTE.into();
            }
            "namespace" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut original.partition else {
                    panic!()
                };
                member.namespace.id = "foreign-logical-database".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut original.partition else { panic!() };
                *epoch = None;
            }
            "engine" => {
                let CollectionTarget::Database(database) = &mut original.target else { panic!() };
                database.engine = Engine::Postgres;
            }
            "database" => {
                let SourceObservation::MssqlReplication { scope, .. } = &mut original.observation else { panic!() };
                scope.database_id = 8;
            }
            "scope" => original.partition = SourcePartition::Target,
            "dataset" => {
                let SourceObservation::MssqlReplication { snapshot, .. } = &mut original.observation else { panic!() };
                snapshot.dataset = SnapshotDataset::WaitCounters;
            }
            "complete" => {
                let SourceObservation::MssqlReplication { snapshot, .. } = &mut original.observation else { panic!() };
                snapshot.sources[0].complete_scope = true;
            }
            _ => unreachable!(),
        }
        assert!(!row(point()).fits_source(&original), "{change}");
        assert!(ReplicationSnapshot::from_source(&original, vec![row(point())]).is_err(), "{change}");
    }
}
#[test]
fn partial_missing_and_unavailable_topology_keep_original_counts_without_empty_health_or_native_invention() {
    let original = source();
    let input = ReplicationSnapshot::from_source(&original, vec![]).unwrap();
    assert!(input.is_valid_read(false));
    assert!(!input.is_valid());
    assert_eq!(input.sources[0].records, 1);
    for state in [SourceState::Partial, SourceState::Unavailable] {
        let mut original = source();
        let SourceObservation::MssqlReplication { snapshot, .. } = &mut original.observation else { panic!() };
        snapshot.records = 0;
        snapshot.sources[0] = if state == SourceState::Partial {
            StateSourceCoverage::observed(MSSQL_HYPERSCALE_REPLICAS_SOURCE.into(), 0, false)
        } else {
            StateSourceCoverage::unavailable(MSSQL_HYPERSCALE_REPLICAS_SOURCE.into(), ErrorCode::PermissionDenied, None)
        };
        let input = ReplicationSnapshot::from_source(&original, vec![]).unwrap();
        assert!(input.is_valid());
        assert_eq!(input.sources[0].state, state);
        assert!(input.mssql_scope.is_some());
        assert!(input.observations.is_empty());
    }
}
#[test]
fn nullable_topology_fields_are_required_and_never_repaired_to_known_absence() {
    for field in [
        "replica_server_name",
        "replica_database_name",
        "is_local",
        "replica_role",
        "native_role",
        "local_database_id",
        "parent_replica_id",
    ] {
        let mut value = serde_json::to_value(point()).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<MssqlHyperscaleReplica>(value).is_err(), "{field}");
    }
    let read = ReplicationSnapshot::from_source(&source(), vec![row(point())]).unwrap();
    let mut value = serde_json::to_value(read).unwrap();
    value.as_object_mut().unwrap().remove("mssql_scope");
    assert!(serde_json::from_value::<ReplicationSnapshot>(value).is_err());
}
