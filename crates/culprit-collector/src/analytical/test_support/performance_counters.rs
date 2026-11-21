//! Native-shaped fixed counters use the existing authentic Operational capture and shared Details codec.
use super::*;
use culprit_core::{Engine, MeasurementScope, ResetMarker};
mod database;
mod resource_governor;
pub use database::{DatabasePerformanceCounterCase, database_performance_counter_evidence};
pub use resource_governor::{ResourceGovernorCounterCase, resource_governor_counter_evidence};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerformanceCounterCase {
    Exact,
    MeasuredZero,
    MissingFields,
    UnknownNativeType,
    NativePartial,
    ClockUnavailable,
    Denied,
}
impl PerformanceCounterCase {
    pub const ALL: [Self; 7] = [
        Self::Exact,
        Self::MeasuredZero,
        Self::MissingFields,
        Self::UnknownNativeType,
        Self::NativePartial,
        Self::ClockUnavailable,
        Self::Denied,
    ];
}

pub fn performance_counter_evidence(case: PerformanceCounterCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut records = Vec::new();
    if case != PerformanceCounterCase::Denied {
        records.push(TelemetryRecord::Operational(
            OperationalObservation::from_counters(culprit_core::source_id::PERFORMANCE_COUNTERS, sample(case)).unwrap(),
        ));
    }
    let count = records.len() as u32;
    records.push(TelemetryRecord::Operational(
        OperationalObservation::from_counters(
            "independent-native-counters",
            NativeCounterSet {
                identity: NativeIdentity::new("unrelated"),
                scope: MeasurementScope::Server,
                counters: vec![NativeCounterSample::gauge(
                    "unrelated",
                    42,
                    CounterUnit::Count,
                    MeasurementScope::Server,
                    "other-native-view",
                    "unrelated",
                )],
                ratios: vec![],
                ratio_gaps: vec![],
                properties: Default::default(),
            },
        )
        .unwrap(),
    ));
    let member = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-instance-namespace"),
        native: NativeIdentity::new("native-instance-a"),
    };
    let source = CollectionDescription {
        target: CollectionTarget::Shared(member.clone()),
        source: "shared-operational-state".into(),
        partition: SourcePartition::InstanceLocal { member: Some(member), epoch: Some("native-process-a".into()) },
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00.000000000Z".into(),
            observed_until: "2020-01-01T12:00:01.000000000Z".into(),
            records: records.len() as u32,
            sources: vec![
                if case == PerformanceCounterCase::Denied {
                    StateSourceCoverage::unavailable(
                        culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
                        culprit_core::ErrorCode::PermissionDenied,
                        None,
                    )
                } else {
                    StateSourceCoverage::observed(
                        culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
                        count,
                        case != PerformanceCounterCase::NativePartial,
                    )
                },
                StateSourceCoverage::collected("independent-native-counters".into(), 1),
            ],
        }),
    };
    assert!(source.partition_is_consistent());
    (source, records)
}

fn sample(case: PerformanceCounterCase) -> NativeCounterSet {
    let mut counters = Vec::new();
    let mut add = |object: &str, name: &str, instance: Option<&str>, code: i32, value: u64| {
        let profile = Engine::Mssql.spec().performance_counters().unwrap();
        let spec = profile.field(object, name, instance).unwrap();
        let native_type =
            if case == PerformanceCounterCase::UnknownNativeType && name == "Granted Workspace Memory (KB)" {
                42
            } else {
                code
            };
        let (nature, unit) = spec.meaning(native_type);
        let descriptor = (object.to_owned(), name.to_owned(), instance.map(str::to_owned));
        counters.push(NativeCounterSample {
            key: digest(&serde_json::to_vec(&descriptor).unwrap()),
            value: MeasurementValue::Integer(if case == PerformanceCounterCase::MeasuredZero {
                "0".into()
            } else {
                value.to_string()
            }),
            unit,
            nature,
            scope: MeasurementScope::Server,
            reset: ResetMarker::Unknown,
            native_object: object.into(),
            native_name: name.into(),
            native_instance: instance.map(str::to_owned),
            native_type: Some(native_type.to_string()),
        });
    };
    add("SQLServer:Memory Manager", "Granted Workspace Memory (KB)", None, 65_792, 9_007_199_254_740_993);
    if case != PerformanceCounterCase::MissingFields {
        add("SQLServer:Memory Manager", "Memory Grants Pending", None, 65_792, 0);
    }
    add("SQLServer:SQL Statistics", "SQL Compilations/sec", None, 272_696_576, 11);
    add("SQLServer:General Statistics", "Temp Tables Creation Rate", None, 272_696_576, 3);
    add("SQLServer:Databases", "Log Bytes Flushed/sec", Some("_Total"), 272_696_576, 9_007_199_254_740_993);
    for instance in ["SQL Plans", "Object Plans"] {
        add("SQLServer:Plan Cache", "Cache Pages", Some(instance), 65_792, 12);
        add("SQLServer:Plan Cache", "Cache Hit Ratio", Some(instance), 537_003_264, 1);
        if case != PerformanceCounterCase::MissingFields || instance != "Object Plans" {
            add("SQLServer:Plan Cache", "Cache Hit Ratio Base", Some(instance), 1_073_939_712, 2);
        }
    }
    let at = (case != PerformanceCounterCase::ClockUnavailable).then(|| "2020-01-01T12:00:00.500000000Z".to_owned());
    let ratios = ["SQL Plans", "Object Plans"]
        .into_iter()
        .map(|instance| {
            let key = |name| {
                counters
                    .iter()
                    .find(|row| row.native_name == name && row.native_instance.as_deref() == Some(instance))
                    .map(|row| row.key.clone())
            };
            NativeCounterRatio {
                kind: CounterRatioKind::PlanCacheHit,
                numerator: key("Cache Hit Ratio"),
                base: key("Cache Hit Ratio Base"),
                support: CounterRatioSupport::Supported,
                observed_at: at.clone(),
            }
        })
        .collect();
    let mut properties = std::collections::BTreeMap::from([
        (PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), PERFORMANCE_COUNTER_PROFILE.into()),
        (PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), counters.len().to_string()),
    ]);
    if let Some(at) = at {
        properties.insert(PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(), at);
    }
    let set = NativeCounterSet {
        identity: NativeIdentity::new("sys.dm_os_performance_counters")
            .qualified("native_object", "sys.dm_os_performance_counters"),
        scope: MeasurementScope::Server,
        counters,
        ratios,
        ratio_gaps: vec![],
        properties,
    };
    Engine::Mssql.spec().performance_counters().unwrap().validate(&set).unwrap();
    set
}
