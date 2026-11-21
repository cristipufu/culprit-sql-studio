//! Synthetic original scheduler rows exercise the existing source/receipt path, never native cost qualification.
use super::*;
use culprit_core::{ErrorCode, MeasurementScope, ResetMarker};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerCase {
    Exact,
    MeasuredZero,
    NativeNull,
    ColumnUnavailable,
    NativePartial,
    UnqualifiedOwner,
    Denied,
    Limited,
    Sql2019Layout,
    Sql2022Layout,
    ModernNulls,
    SpinlockRawPositive,
}
impl SchedulerCase {
    pub const ALL: [Self; 12] = [
        Self::Exact,
        Self::MeasuredZero,
        Self::NativeNull,
        Self::ColumnUnavailable,
        Self::NativePartial,
        Self::UnqualifiedOwner,
        Self::Denied,
        Self::Limited,
        Self::Sql2019Layout,
        Self::Sql2022Layout,
        Self::ModernNulls,
        Self::SpinlockRawPositive,
    ];
}

pub fn instance_resource_scheduler_evidence(case: SchedulerCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut rows) = instance_resource_evidence(InstanceResourceCase::Exact);
    let count = match case {
        SchedulerCase::Denied => 0,
        SchedulerCase::Limited => 257,
        _ => 2,
    };
    for id in 0..count {
        let sample = scheduler(id, case);
        rows.push(TelemetryRecord::Operational(
            OperationalObservation::from_counters(culprit_core::source_id::SCHEDULERS, sample).unwrap(),
        ));
    }
    let header = source.observation.snapshot().unwrap();
    let mut header = header.clone();
    header.records = rows.len() as u32;
    header.sources.push(if case == SchedulerCase::Denied {
        StateSourceCoverage::unavailable(culprit_core::source_id::SCHEDULERS.into(), ErrorCode::PermissionDenied, None)
    } else {
        StateSourceCoverage::observed(
            culprit_core::source_id::SCHEDULERS.into(),
            count,
            !matches!(case, SchedulerCase::NativePartial | SchedulerCase::UnqualifiedOwner),
        )
    });
    if case == SchedulerCase::UnqualifiedOwner {
        source.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
        for coverage in &mut header.sources {
            *coverage = StateSourceCoverage::observed(coverage.source.clone(), coverage.records, false);
        }
    }
    source.observation = SourceObservation::Snapshot(header);
    assert!(source.partition_is_consistent());
    (source, rows)
}

fn scheduler(id: u32, case: SchedulerCase) -> NativeCounterSet {
    let mut sample = NativeCounterSet {
        identity: NativeIdentity::new(id.to_string()).qualified("native_object", MSSQL_SCHEDULER_OBJECT),
        scope: MeasurementScope::Server,
        counters: vec![],
        properties: BTreeMap::new(),
        ratios: vec![],
        ratio_gaps: vec![],
    };
    for field in MSSQL_SCHEDULER_FIELDS {
        match field.kind {
            MssqlNativeFieldKind::Identity => (),
            MssqlNativeFieldKind::IntegerProperty => {
                sample
                    .properties
                    .insert(field.name.into(), if field.name == "cpu_id" { id.to_string() } else { "3".into() });
            }
            MssqlNativeFieldKind::BooleanProperty => {
                sample.properties.insert(field.name.into(), u8::from(field.name == "is_online").to_string());
            }
            MssqlNativeFieldKind::TextProperty => {
                sample
                    .properties
                    .insert(field.name.into(), if id == 0 { "VISIBLE ONLINE" } else { "HIDDEN ONLINE" }.into());
            }
            MssqlNativeFieldKind::UnqualifiedCounter { .. } => {
                sample.properties.insert(field.name.into(), "0".into());
            }
            MssqlNativeFieldKind::Counter { unit, nature } => sample.counters.push(NativeCounterSample {
                key: field.name.into(),
                value: MeasurementValue::Integer(
                    if case == SchedulerCase::MeasuredZero || field.name == "pending_disk_io_count" {
                        "0"
                    } else if field.name == "work_queue_count" {
                        "9007199254740993"
                    } else {
                        "2"
                    }
                    .into(),
                ),
                unit,
                nature,
                scope: MeasurementScope::Server,
                reset: ResetMarker::Unknown,
                native_object: MSSQL_SCHEDULER_OBJECT.into(),
                native_name: field.name.into(),
                native_instance: None,
                native_type: None,
            }),
        }
    }
    if case == SchedulerCase::NativeNull {
        sample.properties.remove("failed_to_create_worker");
        sample.counters.retain(|counter| counter.key != "current_tasks_count");
        sample
            .properties
            .insert("native_null_columns".into(), "[\"failed_to_create_worker\",\"current_tasks_count\"]".into());
    }
    if case == SchedulerCase::ColumnUnavailable {
        sample.counters.retain(|counter| !matches!(counter.key.as_str(), "ideal_workers_limit" | "total_cpu_usage_ms"));
        sample
            .properties
            .insert("unavailable_columns".into(), "[\"ideal_workers_limit\",\"total_cpu_usage_ms\"]".into());
    }
    let unavailable = match case {
        SchedulerCase::Sql2019Layout => Some(15),
        SchedulerCase::Sql2022Layout => Some(16),
        _ => None,
    };
    if let Some(major) = unavailable {
        let names = MSSQL_SCHEDULER_FIELDS
            .iter()
            .filter(|field| field.introduced_in.is_some_and(|since| since > major))
            .map(|field| field.name)
            .collect::<Vec<_>>();
        for name in &names {
            sample.properties.remove(*name);
            sample.counters.retain(|counter| counter.key != *name);
        }
        sample.properties.insert("unavailable_columns".into(), serde_json::to_string(&names).unwrap());
    }
    if case == SchedulerCase::ModernNulls {
        let names = MSSQL_SCHEDULER_FIELDS
            .iter()
            .filter(|field| field.introduced_in.is_some())
            .map(|field| field.name)
            .collect::<Vec<_>>();
        for name in &names {
            sample.properties.remove(*name);
            sample.counters.retain(|counter| counter.key != *name);
        }
        sample.properties.insert("native_null_columns".into(), serde_json::to_string(&names).unwrap());
    }
    if case == SchedulerCase::SpinlockRawPositive {
        for field in MSSQL_SCHEDULER_FIELDS
            .iter()
            .filter(|field| matches!(field.kind, MssqlNativeFieldKind::UnqualifiedCounter { .. }))
        {
            sample.properties.insert(field.name.into(), "7".into());
        }
    }
    assert!(MssqlSchedulerPoint::from_sample(&sample).is_ok());
    sample
}
