//! Controlled native-shaped corpus through the actual pure view, not a live measurement or source approval.
use culprit_collector::analytical::test_support::{StorageCase, storage_evidence};
use culprit_core::telemetry::*;
use std::{collections::BTreeMap, path::PathBuf};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    culprit_core::i18n::scope("en", generate)
}
fn generate() -> Result<(), Box<dyn std::error::Error>> {
    let mut reports = BTreeMap::new();
    for (name, case) in [
        ("exact", StorageCase::Exact),
        ("readonly_offline", StorageCase::ReadOnlyOffline),
        ("unknown", StorageCase::Unknown),
        ("partial", StorageCase::Partial),
        ("unavailable", StorageCase::Denied),
        ("file_census", StorageCase::FileCensus),
        ("file_census_unknown_usage", StorageCase::FileCensusUnknownUsage),
        ("file_census_unknown_basis", StorageCase::FileCensusUnknownBasis),
        ("file_census_partial", StorageCase::FileCensusPartial),
        ("file_census_denied", StorageCase::FileCensusDenied),
        ("file_census_log_denied", StorageCase::FileCensusLogDenied),
        ("file_census_large", StorageCase::FileCensusLarge),
    ] {
        let (source, rows) = storage_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { return Err(DeliveryProblem::Integrity.error()) };
                Ok(row)
            })
            .collect::<culprit_core::Result<Vec<_>>>()?;
        reports.insert(name, culprit_checks::storage::explain(&StorageRead::from_source(&source, rows, true)?)?);
    }
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/storage.sample.json"));
    std::fs::write(path, serde_json::to_string_pretty(&reports)? + "\n")?;
    Ok(())
}
