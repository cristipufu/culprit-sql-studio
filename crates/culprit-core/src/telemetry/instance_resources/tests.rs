use super::*;

fn recorded_instance_point() -> MssqlInstanceResourceObservation {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/instance-resources.native.json"
    )))
    .unwrap()
}
fn recorded_instance_read() -> InstanceResourceRead {
    InstanceResourceRead {
        context: ObservationReadContext::Collector {
            observed_from: "2020-01-01T11:59:59Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
        },
        source: StateSourceCoverage::collected(INSTANCE_RESOURCES_SOURCE.into(), 1),
        observation: Some(Box::new(recorded_instance_point())),
        read_complete: true,
    }
}

#[test]
fn exact_large_memory_zero_unknown_and_independent_low_flags_survive_serialization() {
    let mut point = recorded_instance_point();
    assert!(point.is_valid() && point.has_complete_population());
    point.memory.process_physical_low = Some(true);
    assert!(point.has_complete_population(), "a native low flag changes no population or admission claim");
    let encoded = serde_json::to_string(&point).unwrap();
    assert!(encoded.contains("\"9007199254740993\""));
    let restored: MssqlInstanceResourceObservation = serde_json::from_str(&encoded).unwrap();
    assert_eq!(restored, point);
    assert_eq!(restored.memory.virtual_committed_kib.as_deref(), Some("0"));
    assert!(restored.memory.virtual_available_kib.is_none());
    assert_eq!(restored.memory.process_physical_low, Some(true));
}

#[test]
fn capped_or_unknown_native_prefixes_are_retained_without_complete_membership() {
    for changed in ["schedulers", "grants", "pending", "waits", "memory", "unknowns"] {
        let mut read = recorded_instance_read();
        let point = read.observation.as_deref_mut().unwrap();
        match changed {
            "schedulers" => point.schedulers.population_rows = MAX_INSTANCE_RESOURCE_SCHEDULERS + 1,
            "grants" => point.grant_rows = MAX_INSTANCE_RESOURCE_GRANT_REQUESTS + 1,
            "pending" => {
                point.pending_io.observed_rows = MAX_INSTANCE_RESOURCE_PENDING_IOS + 1;
                point.pending_io.os_pending = Some(point.pending_io.observed_rows.to_string());
            }
            "waits" => point.io_waits.population_rows = MAX_INSTANCE_RESOURCE_WAITING_TASKS + 1,
            "memory" => {
                point.memory.process_rows = 2;
                point.memory.process_physical_low = None;
                point.memory.process_virtual_low = None;
                point.memory.physical_used_kib = None;
                point.memory.virtual_committed_kib = None;
                point.memory.virtual_available_kib = None;
            }
            _ => {
                point.schedulers.unknown_values = 1;
                point.schedulers.runnable_tasks = None;
            }
        }
        assert!(point.is_valid(), "{changed}");
        assert!(!point.has_complete_population(), "{changed}");
        assert!(!read.is_valid(), "a complete native header must not hide {changed}");
        read.source = StateSourceCoverage::observed(INSTANCE_RESOURCES_SOURCE.into(), 1, false);
        assert!(read.is_valid(), "{changed}");
    }
}

#[test]
fn malformed_or_contradictory_saved_native_values_are_rejected_without_repair() {
    for changed in [
        "negative",
        "decimal",
        "leading-zero",
        "overflow",
        "clock",
        "unknown-count",
        "queue-count",
        "io-count",
        "empty-wait",
    ] {
        let mut point = recorded_instance_point();
        match changed {
            "negative" => point.memory.physical_used_kib = Some("-1".into()),
            "decimal" => point.schedulers.runnable_tasks = Some("0.0".into()),
            "leading-zero" => point.schedulers.worker_queue = Some("00".into()),
            "overflow" => point.memory.physical_used_kib = Some((i64::MAX as u64 + 1).to_string()),
            "clock" => point.observed_at = "2020-01-01T12:00:00.1234567891Z".into(),
            "unknown-count" => point.schedulers.unknown_values = point.schedulers.observed_rows + 1,
            "queue-count" => point.schedulers.failed_worker_creations = Some("3".into()),
            "io-count" => point.pending_io.os_pending = Some("1".into()),
            _ => point.io_waits.longest_wait_ms = Some("0".into()),
        }
        let encoded = serde_json::to_string(&point).unwrap();
        let restored: MssqlInstanceResourceObservation = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored, point);
        assert!(!restored.is_valid(), "{changed}");
        assert!(restored.key().is_err(), "{changed}");
    }
}

#[test]
fn original_native_member_and_bounds_are_required_for_shared_archive_scope() {
    let mut read = recorded_instance_read();
    let member = SharedResourceIdentity {
        engine: crate::Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-machine"),
        native: NativeIdentity::new("native-instance"),
    };
    let native = NativeReadContext {
        target: CollectionTarget::Shared(member.clone()),
        partition: SourcePartition::InstanceLocal {
            member: Some(member),
            epoch: Some("original-process-epoch".into()),
        },
        observed_from: "2020-01-01T11:59:59Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        database_writable: None,
    };
    read.context = ObservationReadContext::Native { context: Box::new(native.clone()) };
    assert!(read.is_valid());
    assert!(read.context.native().unwrap().has_qualified_local_owner());
    for changed in ["database", "bounds", "engine", "role-is-not-member"] {
        let mut changed_read = read.clone();
        let ObservationReadContext::Native { context } = &mut changed_read.context else { panic!() };
        match changed {
            "database" => {
                context.target =
                    crate::DatabaseIdentity::server(crate::Engine::Mssql, "selected-db", "fixture.invalid", 1433, None)
                        .into();
                context.partition = SourcePartition::Target;
            }
            "bounds" => context.observed_from = "2020-01-01T12:00:00.1Z".into(),
            "engine" => {
                let CollectionTarget::Shared(resource) = &mut context.target else { panic!() };
                resource.engine = crate::Engine::Mysql;
            }
            _ => {
                context.partition =
                    SourcePartition::InstanceLocal { member: None, epoch: Some("original-process-epoch".into()) }
            }
        }
        if changed == "role-is-not-member" {
            assert!(!context.has_qualified_local_owner());
            // Preserving an unknown member is valid partial context, never an inferred current owner.
        } else {
            assert!(!changed_read.is_valid(), "{changed}");
        }
    }
}

#[test]
fn missing_body_and_unavailable_source_do_not_become_a_healthy_empty_point() {
    let mut read = recorded_instance_read();
    read.observation = None;
    assert!(!read.is_valid());
    read.read_complete = false;
    assert!(read.is_valid());
    read.source =
        StateSourceCoverage::unavailable(INSTANCE_RESOURCES_SOURCE.into(), crate::ErrorCode::PermissionDenied, None);
    read.read_complete = true;
    assert!(read.is_valid());
    read.observation = Some(Box::new(recorded_instance_point()));
    assert!(!read.is_valid());
}
