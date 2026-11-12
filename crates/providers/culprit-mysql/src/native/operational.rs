//! Server totals, InnoDB trouble and native I/O. Shared scope and reset uncertainty remain explicit.
use super::*;
use crate::{connect::Flavor, vars::VariableSet};
use culprit_core::{MeasurementScope, ResetMarker, telemetry::*};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Kind {
    Status,
    InnoDb,
    Waits,
    Files,
    Tables,
    Transactions,
}
pub(crate) const ALL: &[Kind] = &[Kind::Status, Kind::InnoDb, Kind::Waits, Kind::Files, Kind::Tables];
impl Kind {
    pub(crate) fn dataset(self) -> SnapshotDataset {
        if matches!(self, Self::Transactions) { SnapshotDataset::Cleanup } else { SnapshotDataset::Operational }
    }
    pub(crate) fn source(self) -> &'static str {
        match self {
            Self::Status => "global-status",
            Self::InnoDb => "information_schema.innodb_metrics",
            Self::Waits => "performance_schema.events_waits_summary_global_by_event_name",
            Self::Files => "performance_schema.file_summary_by_instance",
            Self::Tables => "performance_schema.table_io_waits_summary_by_table",
            Self::Transactions => "information_schema.innodb_trx",
        }
    }
    pub(crate) fn requires_instrumentation(self) -> bool {
        matches!(self, Self::Waits | Self::Files | Self::Tables)
    }
    pub(crate) fn database_scoped(self) -> bool {
        matches!(self, Self::Tables)
    }
    pub(crate) fn query(self, flavor: Flavor, limit: usize) -> String {
        match self {
            Self::Status => VariableSet::Status.query(&STATUS.iter().map(|spec| spec.name).collect::<Vec<_>>()),
            Self::InnoDb => {
                let enabled = if flavor == Flavor::Mariadb { "ENABLED=1" } else { "STATUS='enabled'" };
                format!(
                    "SELECT NAME AS name, COUNT AS value, TYPE AS native_type, CAST({enabled} AS CHAR) AS enabled
                    FROM information_schema.INNODB_METRICS WHERE NAME IN
                    ('lock_deadlocks','lock_timeouts','trx_rseg_history_len','log_waits') LIMIT {limit}"
                )
            }
            Self::Waits => format!("{WAITS_QUERY} LIMIT {limit}"),
            Self::Files => format!("{FILES_QUERY} LIMIT {limit}"),
            Self::Tables => format!(
                "SELECT OBJECT_NAME AS name, COUNT_FETCH AS fetches, COUNT_INSERT AS inserts,
                COUNT_UPDATE AS updates, COUNT_DELETE AS deletes FROM performance_schema.table_io_waits_summary_by_table
                WHERE OBJECT_SCHEMA=? LIMIT {limit}"
            ),
            Self::Transactions => format!(
                "SELECT trx_id AS id, CAST(trx_mysql_thread_id AS CHAR) AS session_id,
                trx_state AS state, trx_isolation_level AS isolation_level,
                DATE_FORMAT(trx_started,'%Y-%m-%dT%H:%i:%s') AS started_without_offset,
                TIMESTAMPDIFF(MICROSECOND,trx_started,NOW(6)) DIV 1000 AS age_ms,
                trx_rows_locked AS rows_locked, trx_rows_modified AS rows_modified
                FROM information_schema.INNODB_TRX LIMIT {limit}"
            ),
        }
    }
    pub(crate) fn decode(self, row: &Rec, database: &str) -> Result<TelemetryRecord> {
        let scope = if self.database_scoped() { MeasurementScope::Database } else { MeasurementScope::Server };
        let mut properties = BTreeMap::new();
        let mut counters = vec![];
        let id = match self {
            Self::Status => {
                let name = text(row, "variable_name")?;
                let spec = STATUS.iter().find(|spec| spec.name.eq_ignore_ascii_case(&name)).ok_or_else(invalid)?;
                if let Some(value) = row.unsigned("value")? {
                    counters.push(counter(spec.name, value, spec.unit, spec.nature, self.source(), scope));
                }
                name
            }
            Self::InnoDb => {
                let name = text(row, "name")?;
                let enabled = text(row, "enabled")?;
                if !matches!(enabled.as_str(), "0" | "1") {
                    return Err(invalid());
                }
                properties.insert("enabled".into(), enabled.clone());
                properties.insert("native_type".into(), text(row, "native_type")?);
                if enabled == "1" {
                    let nature =
                        if name == "trx_rseg_history_len" { CounterNature::Gauge } else { CounterNature::Cumulative };
                    counters.push(counter(
                        &name,
                        integer(row, "value")?,
                        CounterUnit::Count,
                        nature,
                        self.source(),
                        scope,
                    ));
                }
                name
            }
            Self::Waits => {
                let event = text(row, "event")?;
                let timed = row.native_text("timed")?;
                properties.insert("timed".into(), timed.clone().unwrap_or_else(|| "unknown".into()));
                counters.push(counter(
                    "tasks",
                    integer(row, "tasks")?,
                    CounterUnit::Count,
                    CounterNature::Cumulative,
                    self.source(),
                    scope,
                ));
                if timed.as_deref() == Some("YES") {
                    counters.push(counter(
                        "wait-time",
                        integer(row, "wait_ps")?,
                        CounterUnit::Picoseconds,
                        CounterNature::Cumulative,
                        self.source(),
                        scope,
                    ));
                }
                event
            }
            Self::Files => {
                let name = text(row, "file_name")?;
                properties.insert("event_name".into(), text(row, "event_name")?);
                for (name, unit) in [
                    ("read_count", CounterUnit::Count),
                    ("write_count", CounterUnit::Count),
                    ("read_bytes", CounterUnit::Bytes),
                    ("write_bytes", CounterUnit::Bytes),
                ] {
                    counters.push(counter(
                        name,
                        integer(row, name)?,
                        unit,
                        CounterNature::Cumulative,
                        self.source(),
                        scope,
                    ));
                }
                for (time, count) in [("read_ps", "read_count"), ("write_ps", "write_count")] {
                    let value = integer(row, time)?;
                    if value > 0 || integer(row, count)? == 0 {
                        counters.push(counter(
                            time,
                            value,
                            CounterUnit::Picoseconds,
                            CounterNature::Cumulative,
                            self.source(),
                            scope,
                        ));
                    }
                }
                name
            }
            Self::Tables => {
                for name in ["fetches", "inserts", "updates", "deletes"] {
                    counters.push(counter(
                        name,
                        integer(row, name)?,
                        CounterUnit::Count,
                        CounterNature::Cumulative,
                        self.source(),
                        scope,
                    ));
                }
                text(row, "name")?
            }
            Self::Transactions => {
                for name in ["session_id", "state", "isolation_level", "started_without_offset"] {
                    properties.insert(name.into(), text(row, name)?);
                }
                for (name, unit) in [
                    ("age_ms", CounterUnit::Milliseconds),
                    ("rows_locked", CounterUnit::Count),
                    ("rows_modified", CounterUnit::Count),
                ] {
                    counters.push(counter(name, integer(row, name)?, unit, CounterNature::Gauge, self.source(), scope));
                }
                format!("{}:{}:{}", integer(row, "id")?, text(row, "session_id")?, text(row, "started_without_offset")?)
            }
        };
        let mut identity = NativeIdentity::new(id);
        if self.database_scoped() {
            identity = identity.qualified("schema", database);
        }
        let set = NativeCounterSet { identity, scope, counters, properties };
        if !set.is_valid() {
            return Err(invalid());
        }
        Ok(TelemetryRecord::Operational(OperationalObservation {
            source: self.source().into(),
            key: set.key()?,
            fact: OperationalFact::CounterSet(Box::new(set)),
        }))
    }
}
fn counter(
    name: &str,
    value: u64,
    unit: CounterUnit,
    nature: CounterNature,
    source: &str,
    scope: MeasurementScope,
) -> NativeCounterSample {
    NativeCounterSample {
        nature,
        reset: ResetMarker::Unknown,
        ..NativeCounterSample::gauge(name, value, unit, scope, source, name)
    }
}
struct Status {
    name: &'static str,
    unit: CounterUnit,
    nature: CounterNature,
}
macro_rules! status {
    ($name:literal, $unit:ident, $nature:ident) => {
        Status { name: $name, unit: CounterUnit::$unit, nature: CounterNature::$nature }
    };
}
const STATUS: &[Status] = &[
    status!("Uptime", Seconds, Gauge),
    status!("Connections", Count, Cumulative),
    status!("Aborted_connects", Count, Cumulative),
    status!("Aborted_clients", Count, Cumulative),
    status!("Threads_created", Count, Cumulative),
    status!("Threads_running", Count, Gauge),
    status!("Threads_connected", Count, Gauge),
    status!("Max_used_connections", Count, Gauge),
    status!("Questions", Count, Cumulative),
    status!("Slow_queries", Count, Cumulative),
    status!("Bytes_received", Bytes, Cumulative),
    status!("Bytes_sent", Bytes, Cumulative),
    status!("Created_tmp_disk_tables", Count, Cumulative),
    status!("Created_tmp_tables", Count, Cumulative),
    status!("Innodb_buffer_pool_reads", Count, Cumulative),
    status!("Innodb_buffer_pool_read_requests", Count, Cumulative),
    status!("Innodb_buffer_pool_wait_free", Count, Cumulative),
    status!("Innodb_buffer_pool_bytes_data", Bytes, Gauge),
    status!("Innodb_buffer_pool_bytes_dirty", Bytes, Gauge),
    status!("Innodb_data_pending_reads", Count, Gauge),
    status!("Innodb_data_pending_writes", Count, Gauge),
    status!("Innodb_data_read", Bytes, Cumulative),
    status!("Innodb_data_written", Bytes, Cumulative),
    status!("Innodb_log_waits", Count, Cumulative),
    status!("Innodb_os_log_written", Bytes, Cumulative),
    status!("Innodb_row_lock_current_waits", Count, Gauge),
    status!("Innodb_row_lock_waits", Count, Cumulative),
    status!("Innodb_row_lock_time", Milliseconds, Cumulative),
    status!("Innodb_deadlocks", Count, Cumulative),
    status!("Innodb_history_list_length", Count, Gauge),
];
pub(crate) const WAITS_QUERY: &str =
    "SELECT w.EVENT_NAME AS event, w.COUNT_STAR AS tasks, w.SUM_TIMER_WAIT AS wait_ps, i.TIMED AS timed
    FROM performance_schema.events_waits_summary_global_by_event_name w
    LEFT JOIN performance_schema.setup_instruments i ON i.NAME=w.EVENT_NAME
    WHERE w.COUNT_STAR>0 AND w.EVENT_NAME NOT IN ('idle','wait/io/table/sql/handler')";
pub(crate) const FILES_QUERY: &str = "SELECT FILE_NAME AS file_name, EVENT_NAME AS event_name,
    COUNT_READ AS read_count, SUM_TIMER_READ AS read_ps, SUM_NUMBER_OF_BYTES_READ AS read_bytes,
    COUNT_WRITE AS write_count, SUM_TIMER_WRITE AS write_ps, SUM_NUMBER_OF_BYTES_WRITE AS write_bytes
    FROM performance_schema.file_summary_by_instance WHERE COUNT_READ+COUNT_WRITE>0";
