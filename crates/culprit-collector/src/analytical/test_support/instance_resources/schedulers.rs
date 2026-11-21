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
}
impl SchedulerCase {
    pub const ALL: [Self; 8] = [
        Self::Exact,
        Self::MeasuredZero,
        Self::NativeNull,
        Self::ColumnUnavailable,
        Self::NativePartial,
        Self::UnqualifiedOwner,
        Self::Denied,
        Self::Limited,
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
            MssqlSchedulerFieldKind::Identity => (),
            MssqlSchedulerFieldKind::IntegerProperty => {
                sample
                    .properties
                    .insert(field.name.into(), if field.name == "cpu_id" { id.to_string() } else { "3".into() });
            }
            MssqlSchedulerFieldKind::BooleanProperty => {
                sample.properties.insert(field.name.into(), u8::from(field.name == "is_online").to_string());
            }
            MssqlSchedulerFieldKind::TextProperty => {
                sample
                    .properties
                    .insert(field.name.into(), if id == 0 { "VISIBLE ONLINE" } else { "HIDDEN ONLINE" }.into());
            }
            MssqlSchedulerFieldKind::Counter { unit, nature } => sample.counters.push(NativeCounterSample {
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
    assert!(MssqlSchedulerPoint::from_sample(&sample).is_ok());
    sample
}
