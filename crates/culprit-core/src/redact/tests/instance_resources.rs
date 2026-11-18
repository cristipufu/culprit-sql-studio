use super::*;
use crate::telemetry::*;

fn private_instance_read() -> InstanceResourceRead {
    let member = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("INSTANCE-PRIVATE-namespace")
            .qualified("endpoint", "INSTANCE-PRIVATE-host.example"),
        native: NativeIdentity::new("INSTANCE-PRIVATE-member").qualified("owner", "INSTANCE-PRIVATE-owner"),
    };
    InstanceResourceRead {
        context: ObservationReadContext::Native {
            context: Box::new(NativeReadContext {
                target: CollectionTarget::Shared(member.clone()),
                partition: SourcePartition::InstanceLocal {
                    member: Some(member),
                    epoch: Some("INSTANCE-PRIVATE-process".into()),
                },
                observed_from: "2020-01-01T11:59:59Z".into(),
                observed_until: "2020-01-01T12:00:01Z".into(),
                database_writable: None,
            }),
        },
        source: StateSourceCoverage::collected(INSTANCE_RESOURCES_SOURCE.into(), 1),
        observation: Some(Box::new(
            serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/instance-resources.native.json"
            )))
            .unwrap(),
        )),
        read_complete: true,
    }
}

fn snapshot_with_instance_read(read: InstanceResourceRead) -> Snapshot {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.pressure.get_or_insert_with(crate::Pressure::default).instance_resources = Some(Box::new(read));
    snapshot
}

#[test]
fn shared_instance_points_keep_exact_measurements_and_original_clocks_while_masking_private_scope() {
    let read = private_instance_read();
    assert!(read.is_valid());
    let snapshot = snapshot_with_instance_read(read.clone());
    let original = serde_json::to_value(&snapshot).unwrap();
    let shared = redact_snapshot(&snapshot);
    let retained = shared.pressure.as_ref().unwrap().instance_resources.as_ref().unwrap();
    assert!(retained.is_valid());
    assert_eq!(retained.observation, read.observation);
    assert_eq!(retained.source, read.source);
    assert_eq!(retained.context.bounds(), read.context.bounds());
    assert!(retained.context.native().unwrap().target.database().is_none());
    assert_eq!(retained.observation.as_ref().unwrap().memory.physical_used_kib.as_deref(), Some("9007199254740993"));
    assert_eq!(retained.observation.as_ref().unwrap().memory.virtual_committed_kib.as_deref(), Some("0"));
    assert_eq!(retained.observation.as_ref().unwrap().memory.virtual_available_kib, None);
    let public = serde_json::to_string(&shared).unwrap();
    assert!(!public.contains("INSTANCE-PRIVATE"));
    assert_eq!(serde_json::to_value(redact_snapshot(&shared)).unwrap(), serde_json::to_value(&shared).unwrap());
    assert_eq!(serde_json::to_value(&snapshot).unwrap(), original);
}

#[test]
fn invalid_original_instance_measurements_are_withheld_with_a_persistent_unavailable_marker() {
    let mut read = private_instance_read();
    read.observation.as_deref_mut().unwrap().memory.process_rows = 2;
    assert!(!read.is_valid(), "a non-singleton memory population cannot retain representative flags or values");
    let original = snapshot_with_instance_read(read);
    let shared = redact_snapshot(&original);
    assert!(shared.pressure.as_ref().unwrap().instance_resources.is_none());
    let gap = shared.sources.iter().find(|source| source.source == INSTANCE_RESOURCES_SOURCE).unwrap();
    assert_eq!(gap.state, crate::SourceState::Unavailable);
    assert_eq!(gap.reason, Some(crate::t!("view.instance_resources.invalid")));
    assert_eq!(serde_json::to_value(redact_snapshot(&shared)).unwrap(), serde_json::to_value(&shared).unwrap());
    assert!(original.pressure.as_ref().unwrap().instance_resources.is_some());
}
