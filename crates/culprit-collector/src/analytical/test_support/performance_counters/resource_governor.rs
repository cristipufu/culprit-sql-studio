//! Manufactured original counter evidence; no native instrumentation, workload, ownership or cost claim.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceGovernorCounterCase {
    Exact,
    Zero,
    Unmapped,
    Partial,
    UnknownNativeType,
    Denied,
    NoRows,
}
impl ResourceGovernorCounterCase {
    pub const ALL: [Self; 7] =
        [Self::Exact, Self::Zero, Self::Unmapped, Self::Partial, Self::UnknownNativeType, Self::Denied, Self::NoRows];
}
pub fn resource_governor_counter_evidence(
    case: ResourceGovernorCounterCase,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut rows) = performance_counter_evidence(if case == ResourceGovernorCounterCase::Denied {
        PerformanceCounterCase::Denied
    } else {
        PerformanceCounterCase::Exact
    });
    let epoch = digest(b"resource-governor-original-process");
    let SourcePartition::InstanceLocal { epoch: scope_epoch, .. } = &mut source.partition else { panic!() };
    *scope_epoch = Some(epoch.clone());
    let CollectionTarget::Shared(owner) = &source.target else { panic!() };
    let instance = owner.telemetry_id().unwrap();
    if case != ResourceGovernorCounterCase::Denied {
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(base), .. }) =
            &mut rows[0]
        else {
            panic!()
        };
        base.properties.insert(MSSQL_RESOURCE_GOVERNOR_REQUEST_PROPERTY.into(), "true".into());
        base.properties.insert("resource_governor_instance_id".into(), instance.clone());
        base.properties.insert("resource_governor_instance_epoch".into(), epoch.clone());
        base.properties.insert(
            "resource_governor_native_rows".into(),
            if case == ResourceGovernorCounterCase::NoRows { "0" } else { "7" }.into(),
        );
        if case != ResourceGovernorCounterCase::NoRows {
            for kind in
                [MssqlResourceGovernorCounterKind::WorkloadGroup, MssqlResourceGovernorCounterKind::ResourcePool]
            {
                let sample = sample(kind, case, &instance, &epoch);
                rows.push(TelemetryRecord::Operational(
                    OperationalObservation::from_counters(culprit_core::source_id::PERFORMANCE_COUNTERS, sample)
                        .unwrap(),
                ));
            }
        }
        let count = rows.iter().filter(|record| {
            matches!(record, TelemetryRecord::Operational(row) if row.source == culprit_core::source_id::PERFORMANCE_COUNTERS)
        }).count() as u32;
        let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
        header.records = rows.len() as u32;
        header.sources[0] = StateSourceCoverage::observed(
            culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
            count,
            !matches!(
                case,
                ResourceGovernorCounterCase::Unmapped
                    | ResourceGovernorCounterCase::Partial
                    | ResourceGovernorCounterCase::NoRows
            ),
        );
    }
    assert!(source.partition_is_consistent());
    (source, rows)
}
fn sample(
    kind: MssqlResourceGovernorCounterKind,
    case: ResourceGovernorCounterCase,
    instance: &str,
    epoch: &str,
) -> NativeCounterSet {
    let name = match kind {
        MssqlResourceGovernorCounterKind::WorkloadGroup => "same-as-database-label",
        MssqlResourceGovernorCounterKind::ResourcePool => "same-as-elastic-pool-label",
    };
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    let family = match kind {
        MssqlResourceGovernorCounterKind::WorkloadGroup => PerformanceCounterFamily::WorkloadGroup,
        MssqlResourceGovernorCounterKind::ResourcePool => PerformanceCounterFamily::ResourcePool,
    };
    let object = format!("SQLServer:{}", kind.object_family());
    let counters = profile
        .investigation_fields
        .iter()
        .filter(|spec| spec.family == family)
        .enumerate()
        .map(|(index, spec)| {
            let code = if case == ResourceGovernorCounterCase::UnknownNativeType {
                42
            } else if spec.nature == CounterNature::Cumulative {
                272_696_576
            } else {
                65_792
            };
            let (nature, unit) = spec.meaning(code);
            NativeCounterSample {
                key: digest(format!("{object}:{}:{name}", spec.native_name).as_bytes()),
                value: MeasurementValue::Integer(
                    if case == ResourceGovernorCounterCase::Zero || index == 0 { "0" } else { "9007199254740993" }
                        .into(),
                ),
                unit,
                nature,
                scope: MeasurementScope::Server,
                reset: ResetMarker::Unknown,
                native_object: object.clone(),
                native_name: spec.native_name.into(),
                native_instance: Some(name.into()),
                native_type: Some(code.to_string()),
            }
        })
        .collect::<Vec<_>>();
    let mut properties = [
        (PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), MSSQL_RESOURCE_GOVERNOR_COUNTER_PROFILE.into()),
        (PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(), "2020-01-01T12:00:00.500000000Z".into()),
        (PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), counters.len().to_string()),
        ("resource_name".into(), name.into()),
    ]
    .into_iter()
    .collect::<std::collections::BTreeMap<String, String>>();
    if case != ResourceGovernorCounterCase::Unmapped {
        properties.insert("pool_id".into(), "2".into());
        if kind == MssqlResourceGovernorCounterKind::WorkloadGroup {
            properties.insert("group_id".into(), "3".into());
        }
        properties.insert("statistics_started_without_offset".into(), "2019-12-31T22:00:00.0000000".into());
    }
    NativeCounterSet {
        identity: NativeIdentity::new(digest(name.as_bytes()))
            .qualified("kind", MSSQL_RESOURCE_GOVERNOR_COUNTER_KIND)
            .qualified("native_object", "sys.dm_os_performance_counters")
            .qualified("resource_kind", kind.key())
            .qualified("resource_governor_instance_id", instance)
            .qualified("resource_governor_instance_epoch", epoch),
        scope: MeasurementScope::Server,
        counters,
        ratios: vec![],
        ratio_gaps: vec![],
        properties,
    }
}
