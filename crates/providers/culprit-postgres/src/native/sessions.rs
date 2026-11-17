//! A single database statistics row supplies session counters, its native reset and its own observation clock.
use super::*;
use culprit_core::{ConnectionOutcomeObservation, ConnectionOutcomeSnapshot, DatabaseIdentity, telemetry::*};
use serde_json::Value;
use std::time::SystemTime;

pub(crate) const SOURCE: &str = "pg_stat_database";

pub(crate) fn values_sql(alias: &str) -> String {
    format!(
        "pg_catalog.to_jsonb({alias}) || pg_catalog.jsonb_build_object('counter_observed_at',{},'native_postmaster_started_at',{},'native_server_version_num',pg_catalog.current_setting('server_version_num'),'native_block_bytes',pg_catalog.current_setting('block_size'))",
        iso("pg_catalog.clock_timestamp()"),
        iso("pg_catalog.pg_postmaster_start_time()")
    )
}

fn query() -> String {
    format!(
        "SELECT '{SOURCE}'::text AS native_source, ({})::text AS native_values FROM pg_catalog.pg_stat_database s WHERE s.datid=(SELECT oid FROM pg_catalog.pg_database WHERE datname=pg_catalog.current_database()) LIMIT 2",
        values_sql("s")
    )
}

/// The caller retains the existing borrowed client/reader. This function drains one already-budgeted statement;
/// it neither opens/closes another native owner nor uses an earlier probe's process age as the counter lifetime.
pub(crate) async fn acquire<R: QueryRead>(
    reader: &mut R,
    target: DatabaseIdentity,
) -> Result<ConnectionOutcomeSnapshot> {
    let from = SystemTime::now();
    let rows = reader.query(&query(), 2).await?;
    if rows.len() != 1 {
        return Err(invalid());
    }
    let row = decode(&rows[0], &target.database)?;
    let snapshot = ConnectionOutcomeSnapshot {
        target: target.into(),
        context: ObservationReadContext::collector(from, SystemTime::now())?,
        sources: vec![StateSourceCoverage::observed(SOURCE.into(), 1, true)],
        observations: vec![row],
    };
    snapshot.is_valid().then_some(snapshot).ok_or_else(invalid)
}

pub(crate) fn decode(row: &Row, database: &str) -> Result<ConnectionOutcomeObservation> {
    if text(row, "native_source")? != SOURCE {
        return Err(invalid());
    }
    let values: Value = serde_json::from_str(&text(row, "native_values")?).map_err(|_| invalid())?;
    if values.get("datname").and_then(Value::as_str) != Some(database) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let database_oid =
        values.get("datid").and_then(Value::as_u64).filter(|oid| *oid > 0).ok_or_else(invalid)?.to_string();
    let started_at = values.get("native_postmaster_started_at").and_then(Value::as_str).ok_or_else(invalid)?;
    let started_at = canonical_time(started_at)?;
    let version = values
        .get("native_server_version_num")
        .and_then(Value::as_str)
        .and_then(|version| version.parse::<u32>().ok())
        .ok_or_else(invalid)?;
    let block_bytes = values
        .get("native_block_bytes")
        .and_then(Value::as_str)
        .and_then(|bytes| bytes.parse::<u32>().ok())
        .filter(|bytes| *bytes > 0)
        .ok_or_else(invalid)?;
    let record = super::operational::decode_values(
        SOURCE.into(),
        values,
        super::operational::CounterReadContext {
            database_oid: &database_oid,
            started_at: &started_at,
            block_bytes,
            version,
        },
    )?;
    let TelemetryRecord::Operational(row) = record else { return Err(invalid()) };
    ConnectionOutcomeObservation::from_operational(&row)
}

#[cfg(test)]
pub(crate) mod tests;
