//! Controlled native-shaped points through the same common view; no native cost or current-owner claim.
use culprit_collector::analytical::test_support::{
    InstanceResourceCase, ProcessCpuCase, SchedulerCase, instance_resource_cpu_evidence, instance_resource_evidence,
    instance_resource_scheduler_evidence,
};
use culprit_core::telemetry::*;
use std::{collections::BTreeMap, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    culprit_core::i18n::scope("en", generate)
}
fn generate() -> Result<(), Box<dyn std::error::Error>> {
    let mut cases: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for (name, case) in [
        ("exact", InstanceResourceCase::Exact),
        ("zero", InstanceResourceCase::Zero),
        ("unreported", InstanceResourceCase::Unreported),
        ("pressure", InstanceResourceCase::Pressure),
        ("truncated", InstanceResourceCase::Truncated),
        ("unknown", InstanceResourceCase::UnknownFlags),
        ("denied", InstanceResourceCase::Denied),
    ] {
        let (source, rows) = instance_resource_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { return Err(DeliveryProblem::Integrity.error()) };
                Ok(row)
            })
            .collect::<culprit_core::Result<Vec<_>>>()?;
        let read = InstanceResourceRead::from_source(&source, &rows, true)?;
        let report = culprit_checks::instance_resources::explain(&read)?;
        cases.insert(name.into(), serde_json::json!({"read":read,"report":report}));
    }
    for (name, case) in [
        ("cpu_exact", ProcessCpuCase::Exact),
        ("cpu_zero", ProcessCpuCase::Zero),
        ("cpu_null", ProcessCpuCase::Null),
        ("cpu_partial", ProcessCpuCase::Partial),
        ("cpu_denied", ProcessCpuCase::Denied),
    ] {
        let (source, rows) = instance_resource_cpu_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| match row {
                TelemetryRecord::Operational(row) => Ok(row),
                _ => Err(DeliveryProblem::Integrity.error()),
            })
            .collect::<culprit_core::Result<Vec<_>>>()?;
        let read = InstanceResourceRead::from_source(&source, &rows, true)?;
        let report = culprit_checks::instance_resources::explain_source(&source, &rows, true)?;
        cases.insert(name.into(), serde_json::json!({"read":read,"report":report}));
    }
    for (name, case) in [
        ("schedulers_exact", SchedulerCase::Exact),
        ("schedulers_zero", SchedulerCase::MeasuredZero),
        ("schedulers_null", SchedulerCase::NativeNull),
        ("schedulers_unavailable", SchedulerCase::ColumnUnavailable),
        ("schedulers_partial", SchedulerCase::NativePartial),
        ("schedulers_owner_unknown", SchedulerCase::UnqualifiedOwner),
        ("schedulers_denied", SchedulerCase::Denied),
        ("schedulers_limited", SchedulerCase::Limited),
        ("schedulers_sql2019", SchedulerCase::Sql2019Layout),
        ("schedulers_sql2022", SchedulerCase::Sql2022Layout),
        ("schedulers_modern_nulls", SchedulerCase::ModernNulls),
        ("schedulers_spinlock_raw_positive", SchedulerCase::SpinlockRawPositive),
    ] {
        let (source, rows) = instance_resource_scheduler_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| match row {
                TelemetryRecord::Operational(row) => Ok(row),
                _ => Err(DeliveryProblem::Integrity.error()),
            })
            .collect::<culprit_core::Result<Vec<_>>>()?;
        let read = InstanceResourceRead::from_source(&source, &rows, true)?;
        let report = culprit_checks::instance_resources::explain_source(&source, &rows, true)?;
        cases.insert(name.into(), serde_json::json!({"read":read,"report":report}));
    }
    for (name, case) in [
        ("numa_exact", culprit_collector::analytical::test_support::NumaNodeCase::Exact),
        ("numa_zero", culprit_collector::analytical::test_support::NumaNodeCase::Zero),
        ("numa_null", culprit_collector::analytical::test_support::NumaNodeCase::NativeNull),
        ("numa_unavailable", culprit_collector::analytical::test_support::NumaNodeCase::ColumnUnavailable),
        ("numa_partial", culprit_collector::analytical::test_support::NumaNodeCase::PartialMemory),
        ("numa_cpu_denied", culprit_collector::analytical::test_support::NumaNodeCase::CpuDenied),
        ("numa_both_denied", culprit_collector::analytical::test_support::NumaNodeCase::BothDenied),
        ("numa_limited", culprit_collector::analytical::test_support::NumaNodeCase::Limited),
    ] {
        let (source, records) = culprit_collector::analytical::test_support::numa_node_evidence(case);
        let rows = records
            .into_iter()
            .map(|row| match row {
                TelemetryRecord::Operational(row) => Ok(row),
                _ => Err(DeliveryProblem::Integrity.error()),
            })
            .collect::<culprit_core::Result<Vec<_>>>()?;
        let report = culprit_checks::instance_resources::explain_source(&source, &rows, true)?;
        cases.insert(name.into(), serde_json::json!({"source":source,"report":report}));
    }
    for case in culprit_collector::analytical::test_support::ProcessCpuIntervalCase::ALL {
        let [first, second] = culprit_collector::analytical::test_support::process_cpu_interval_originals(case);
        let mut report = culprit_checks::instance_resources::explain_captured(&second)?;
        report.process_cpu_originals = Some(vec![
            *culprit_checks::instance_resources::explain_captured(&first)?.process_cpu.unwrap(),
            *report.process_cpu.take().unwrap(),
        ]);
        report.process_cpu_interval =
            Some(Box::new(culprit_checks::instance_resources::compare_cpu_originals(&first, &second)?));
        cases.insert(format!("cpu_profile_{case:?}").to_lowercase(), serde_json::json!({"read":null,"report":report}));
    }
    let path = std::env::args_os().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/instance-resources.sample.json")
    });
    std::fs::write(path, serde_json::to_string_pretty(&cases)? + "\n")?;
    Ok(())
}
