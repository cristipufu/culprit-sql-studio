//! One native channel read for the app and export; server-wide channels never become database-specific lag.
use super::*;
use crate::connect::Flavor;
use culprit_core::{MeasurementScope, telemetry::*};
use std::collections::BTreeMap;
pub(crate) const SOURCE: &str = "replication-channels";
pub(crate) fn query(flavor: Flavor, version: &str) -> &'static str {
    match flavor {
        Flavor::Mysql if culprit_core::version_at_least(version, [8, 0, 22]) => "SHOW REPLICA STATUS",
        Flavor::Mysql => "SHOW SLAVE STATUS",
        Flavor::Mariadb if culprit_core::version_at_least(version, [10, 5, 1]) => "SHOW ALL REPLICAS STATUS",
        Flavor::Mariadb => "SHOW ALL SLAVES STATUS",
    }
}
fn column<'a>(row: &Rec, names: &'a [&str]) -> Result<&'a str> {
    names.iter().copied().find(|name| row.has_column(name)).ok_or_else(invalid)
}
pub(crate) fn decode(row: &Rec) -> Result<TelemetryRecord> {
    let mut properties = BTreeMap::new();
    for (name, alternatives) in [
        ("channel", &["channel_name", "connection_name"][..]),
        ("source_host", &["source_host", "master_host"]),
        ("io_running", &["replica_io_running", "slave_io_running"]),
        ("sql_running", &["replica_sql_running", "slave_sql_running"]),
    ] {
        if let Some(value) = row.native_text(column(row, alternatives)?)? {
            properties.insert(name.into(), value);
        }
    }
    let channel = properties.get("channel").cloned().ok_or_else(invalid)?;
    let mut counters = vec![];
    for (name, alternatives, unit) in [
        ("lag-seconds", &["seconds_behind_source", "seconds_behind_master"][..], CounterUnit::Seconds),
        ("configured-delay", &["sql_delay"], CounterUnit::Seconds),
        ("relay-log-bytes", &["relay_log_space"], CounterUnit::Bytes),
        ("io-error-code", &["last_io_errno"], CounterUnit::Count),
        ("sql-error-code", &["last_sql_errno"], CounterUnit::Count),
    ] {
        if let Some(value) = row.unsigned(column(row, alternatives)?)? {
            counters.push(NativeCounterSample::gauge(name, value, unit, MeasurementScope::Server, SOURCE, name));
        }
    }
    let set = NativeCounterSet {
        identity: NativeIdentity::new(serde_json::to_string(&channel).map_err(|_| invalid())?),
        scope: MeasurementScope::Server,
        properties,
        counters,
    };
    let fact = ReplicationFact::Native(Box::new(NativeReplicationSample::new(set)?));
    Ok(TelemetryRecord::Replication(ReplicationObservation { source: SOURCE.into(), fact }))
}
