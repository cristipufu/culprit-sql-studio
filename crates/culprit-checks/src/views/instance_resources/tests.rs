use super::*;

fn recorded_instance_read() -> InstanceResourceRead {
    InstanceResourceRead {
        context: ObservationReadContext::Collector {
            observed_from: "2020-01-01T11:59:59Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
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
fn field<'a>(report: &'a InstanceResourceReport, key: &str) -> &'a InstanceResourceField {
    report.groups.iter().flat_map(|group| &group.fields).find(|field| field.key == key).unwrap()
}

#[test]
fn shared_native_points_keep_exact_independent_memory_and_io_values_without_a_database_grade() {
    let mut read = recorded_instance_read();
    let point = read.observation.as_deref_mut().unwrap();
    point.memory.process_physical_low = Some(true);
    point.pending_io.observed_rows = 3;
    point.pending_io.os_pending = Some("1".into());
    point.pending_io.completion_pending = Some("2".into());
    point.io_waits.population_rows = 1;
    point.io_waits.observed_rows = 1;
    point.io_waits.longest_wait_ms = Some("9007199254740993".into());
    let report = explain(&read).unwrap();
    assert_eq!(report.source_state, SourceState::Collected);
    assert_eq!(report.native_populations_complete, Some(true));
    assert_eq!(field(&report, "physical_used_kib").value.as_deref(), Some("9007199254740993"));
    assert_eq!(field(&report, "virtual_committed_kib").value.as_deref(), Some("0"));
    assert_eq!(field(&report, "virtual_available_kib").value, None);
    assert_eq!(field(&report, "process_physical_low").value.as_deref(), Some("1"));
    assert_eq!(field(&report, "os_pending").value.as_deref(), Some("1"));
    assert_eq!(field(&report, "completion_pending").value.as_deref(), Some("2"));
    assert_eq!(field(&report, "longest_wait_ms").value.as_deref(), Some("9007199254740993"));
    assert_eq!(report.groups.len(), 6);
    assert_eq!(report.context, Some(read.context.clone()));
    let restored = serde_json::from_slice(&serde_json::to_vec(&read).unwrap()).unwrap();
    assert_eq!(explain(&restored).unwrap(), report);
}

#[test]
fn incomplete_native_prefix_preserves_accessible_positives_without_empty_or_complete_population_claims() {
    let mut read = recorded_instance_read();
    let point = read.observation.as_deref_mut().unwrap();
    point.schedulers.population_rows = MAX_INSTANCE_RESOURCE_SCHEDULERS + 1;
    point.schedulers.runnable_tasks = Some("4".into());
    point.memory.system_low = None;
    point.memory.unknown_system_flags = 1;
    read.source = StateSourceCoverage::observed(INSTANCE_RESOURCES_SOURCE.into(), 1, false);
    let report = explain(&read).unwrap();
    assert_eq!(report.native_source, Some(read.source.clone()));
    assert_eq!(report.source_state, SourceState::Partial);
    assert_eq!(report.native_populations_complete, Some(false));
    assert_eq!(field(&report, "runnable_tasks").value.as_deref(), Some("4"));
    assert_eq!(field(&report, "system_low").value, None);
    assert!(!report.groups.is_empty());
}

#[test]
fn warehouse_body_loss_is_partial_while_original_native_count_and_visibility_remain_unchanged() {
    let mut read = recorded_instance_read();
    read.observation = None;
    read.read_complete = false;
    let report = explain(&read).unwrap();
    assert_eq!(report.source_state, SourceState::Partial);
    assert_eq!(report.native_source.as_ref().unwrap().state, SourceState::Collected);
    assert_eq!(report.native_source.as_ref().unwrap().records, 1);
    assert_eq!(report.native_populations_complete, None);
    assert!(report.groups.is_empty());
    assert!(report.observed_at.is_none());
    read.source = StateSourceCoverage::unavailable(
        INSTANCE_RESOURCES_SOURCE.into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    );
    let denied = explain(&read).unwrap();
    assert_eq!(denied.source_state, SourceState::Unavailable);
    assert!(denied.groups.is_empty());
}

#[test]
fn contradictory_native_totals_and_fabricated_scope_are_rejected_before_presentation() {
    let mut read = recorded_instance_read();
    read.observation.as_deref_mut().unwrap().pending_io.os_pending = Some("1".into());
    assert!(explain(&read).is_err());
    let mut read = recorded_instance_read();
    read.source.source = "another-source".into();
    assert!(explain(&read).is_err());
}
