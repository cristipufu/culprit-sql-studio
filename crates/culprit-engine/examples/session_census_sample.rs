//! Controlled native-shaped cases through the actual pure view; no live owner or source-cost claim.
use culprit_collector::analytical::test_support::{SessionCensusCase, session_census_evidence};
use culprit_core::telemetry::*;
use std::{collections::BTreeMap, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    culprit_core::i18n::scope("en", generate)
}
fn generate() -> Result<(), Box<dyn std::error::Error>> {
    let mut cases = BTreeMap::new();
    for (name, case) in [
        ("idle", SessionCensusCase::Idle),
        ("dormant", SessionCensusCase::Dormant),
        ("mars", SessionCensusCase::Mars),
        ("unknown", SessionCensusCase::Unknown),
        ("partial", SessionCensusCase::Partial),
        ("unavailable", SessionCensusCase::Unavailable),
        ("empty", SessionCensusCase::Empty),
    ] {
        let (source, rows) = session_census_evidence(case);
        let observations = rows
            .into_iter()
            .map(|row| match row {
                TelemetryRecord::Activity(row) => Ok(*row),
                _ => Err(DeliveryProblem::Integrity.error()),
            })
            .collect::<culprit_core::Result<Vec<_>>>()?;
        let read = ActivityCensusRead::new(source, observations, true)?;
        let report = culprit_checks::session_census::explain(&read)?;
        cases.insert(name, serde_json::json!({ "read": read, "report": report }));
    }
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/session-census.sample.json"));
    std::fs::write(path, serde_json::to_string_pretty(&cases)? + "\n")?;
    Ok(())
}
