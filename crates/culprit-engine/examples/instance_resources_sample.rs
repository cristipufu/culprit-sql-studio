//! Controlled native-shaped points through the same common view; no native cost or current-owner claim.
use culprit_collector::analytical::test_support::{
    InstanceResourceCase, ProcessCpuCase, instance_resource_cpu_evidence, instance_resource_evidence,
};
use culprit_core::telemetry::*;
use std::{collections::BTreeMap, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    culprit_core::i18n::scope("en", generate)
}
fn generate() -> Result<(), Box<dyn std::error::Error>> {
    let mut cases = BTreeMap::new();
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
        cases.insert(name, serde_json::json!({"read":read,"report":report}));
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
        cases.insert(name, serde_json::json!({"read":read,"report":report}));
    }
    let path = std::env::args_os().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/instance-resources.sample.json")
    });
    std::fs::write(path, serde_json::to_string_pretty(&cases)? + "\n")?;
    Ok(())
}
