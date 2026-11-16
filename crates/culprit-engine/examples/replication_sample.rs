//! Render controlled native replication facts with the same pure view used by scans and archived analysis.
use culprit_collector::analytical::test_support::{ReplicationCase, replication_evidence};
use culprit_core::{ReplicationReport, telemetry::*};
use std::{collections::BTreeMap, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    culprit_core::i18n::scope("en", generate)
}

fn generate() -> Result<(), Box<dyn std::error::Error>> {
    use ReplicationCase::*;
    let cases = [
        ("postgres_sender", PostgresSender),
        ("postgres_standby_paused", PostgresStandbyPaused),
        ("postgres_standby_requested", PostgresStandbyRequested),
        ("postgres_slots", PostgresSlots),
        ("postgres_legacy_slot", PostgresLegacySlot),
        ("mysql_channel", MySqlChannel),
        ("mariadb_channel", MariaDbChannel),
        ("mssql_availability_group", MssqlAvailabilityGroup),
        ("mssql_geo_link", MssqlGeoLink),
        ("partial", Partial),
        ("empty", Empty),
        ("unavailable", Unavailable),
        ("not_applicable", NotApplicable),
        ("unqualified_owner", UnqualifiedOwner),
    ];
    let mut reports = BTreeMap::new();
    for (name, case) in cases {
        reports.insert(name, culprit_checks::replication::explain(&snapshot(case)?, true)?);
    }
    reports.insert("limited", limited()?);
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/replication.sample.json"));
    std::fs::write(output, serde_json::to_string_pretty(&reports)? + "\n")?;
    Ok(())
}

fn snapshot(case: ReplicationCase) -> culprit_core::Result<ReplicationSnapshot> {
    let (source, records) = replication_evidence(case);
    let rows = records
        .into_iter()
        .map(|record| {
            let TelemetryRecord::Replication(row) = record else { return Err(DeliveryProblem::Integrity.error()) };
            Ok(row)
        })
        .collect::<culprit_core::Result<Vec<_>>>()?;
    ReplicationSnapshot::from_source(&source, rows)
}

fn limited() -> culprit_core::Result<ReplicationReport> {
    // Two UI pages exercise presentation limits without repeating hundreds of fully rendered field tables.
    const SAMPLE_DETAIL_LIMIT: usize = 16;
    let mut snapshot = snapshot(ReplicationCase::PostgresSlots)?;
    let template = snapshot.observations.iter().find(|row| matches!(&row.fact,
        ReplicationFact::Native(sample) if sample.measurements.properties.get("kind").map(String::as_str) == Some("slot")))
        .ok_or_else(|| DeliveryProblem::Integrity.error())?.clone();
    let mut rows = Vec::new();
    for index in 0..300 {
        let mut row = template.clone();
        let ReplicationFact::Native(sample) = &mut row.fact else { return Err(DeliveryProblem::Integrity.error()) };
        let name = format!("sample_slot_{index}");
        sample.measurements.identity.qualifiers.insert("native_id".into(), name.clone());
        sample.measurements.properties.insert("name".into(), name.clone());
        sample.measurements.properties.insert("slot_name".into(), name.clone());
        sample.measurements.properties.insert("active".into(), "false".into());
        sample.measurements.properties.insert("wal_status".into(), "reserved".into());
        sample.measurements.properties.insert("synced".into(), "false".into());
        sample.measurements.properties.insert("conflicting".into(), "false".into());
        sample.measurements.properties.remove("invalidation_reason");
        sample.measurements.properties.remove("active_pid");
        sample.measurements.properties.remove("backend_start");
        sample.session = None;
        sample.unresolved_session_id = None;
        sample.slot_name = Some(name);
        if index == 299 {
            sample.measurements.properties.insert("wal_status".into(), "lost".into());
            sample.measurements.properties.insert("invalidation_reason".into(), "wal_removed".into());
            sample.measurements.counters.retain(|counter| counter.key != "safe_wal_size");
        }
        sample.key = sample.measurements.key()?;
        rows.push(row);
    }
    snapshot.sources = vec![StateSourceCoverage::observed(template.source, rows.len() as u32, false)];
    snapshot.observations = rows;
    culprit_checks::replication::explain_bounded(&snapshot, true, SAMPLE_DETAIL_LIMIT)
}
