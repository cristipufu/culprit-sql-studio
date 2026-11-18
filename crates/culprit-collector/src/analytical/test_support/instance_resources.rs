use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceResourceCase {
    Exact,
    Zero,
    Unreported,
    Pressure,
    Truncated,
    UnknownFlags,
    Denied,
}
impl InstanceResourceCase {
    pub const ALL: [Self; 7] =
        [Self::Exact, Self::Zero, Self::Unreported, Self::Pressure, Self::Truncated, Self::UnknownFlags, Self::Denied];
}

pub fn instance_resource_evidence(case: InstanceResourceCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, _) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let mut point: MssqlInstanceResourceObservation = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/instance-resources.native.json"
    )))
    .unwrap();
    // Reuse the canonical SharedInstance owner; timed native points keep their own controlled read bounds.
    match case {
        InstanceResourceCase::Exact | InstanceResourceCase::Denied => (),
        InstanceResourceCase::Zero => {
            point.memory.physical_used_kib = Some("0".into());
            point.memory.system_total_kib = Some("0".into());
            point.memory.system_available_kib = Some("0".into());
        }
        InstanceResourceCase::Unreported => {
            point.memory.physical_used_kib = None;
            point.memory.virtual_committed_kib = None;
            point.memory.virtual_available_kib = None;
            point.memory.system_total_kib = None;
            point.memory.system_available_kib = None;
        }
        InstanceResourceCase::Pressure => {
            point.memory.process_physical_low = Some(true);
            point.grant_rows = 1;
            point.grant_waiters = 1;
        }
        InstanceResourceCase::Truncated => {
            point.schedulers.population_rows = MAX_INSTANCE_RESOURCE_SCHEDULERS + 1;
            point.grant_rows = MAX_INSTANCE_RESOURCE_GRANT_REQUESTS + 1;
            point.io_waits.population_rows = MAX_INSTANCE_RESOURCE_WAITING_TASKS + 1;
        }
        InstanceResourceCase::UnknownFlags => {
            point.memory.process_virtual_low = None;
            point.memory.unknown_process_flags = 1;
        }
    }
    let mut rows = Vec::new();
    let coverage = if case == InstanceResourceCase::Denied {
        StateSourceCoverage::unavailable(
            INSTANCE_RESOURCES_SOURCE.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else {
        let complete = point.has_complete_population();
        assert!(point.is_valid());
        rows.push(TelemetryRecord::Operational(OperationalObservation {
            source: INSTANCE_RESOURCES_SOURCE.into(),
            key: point.key().unwrap(),
            fact: OperationalFact::MssqlInstanceResources(Box::new(point)),
        }));
        StateSourceCoverage::observed(INSTANCE_RESOURCES_SOURCE.into(), 1, complete)
    };
    let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
    header.observed_from = "2020-01-01T11:59:59Z".into();
    header.observed_until = "2020-01-01T12:00:01Z".into();
    header.records = rows.len() as u32;
    header.sources = vec![coverage];
    assert!(header.is_valid());
    assert!(source.partition_is_consistent());
    (source, rows)
}
