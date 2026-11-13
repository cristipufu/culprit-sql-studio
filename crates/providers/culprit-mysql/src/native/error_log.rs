//! Retained native diagnostics, never a complete event history or a link to a currently running connection.
//!
//! MySQL 8.0.22+ exposes a fixed-size in-memory buffer. An unfiltered table scan takes the native read lock for
//! one copied entry at a time; indexed predicates may scan several entries while holding that lock. Keep this
//! projection free of predicates, sorting, joins and JSON evaluation. Free-form DATA can contain application SQL,
//! secrets or personal data and is deliberately not selected. Row/byte/time caps bound acquisition, not historical
//! coverage or production-load qualification.
//! https://dev.mysql.com/doc/refman/8.4/en/performance-schema-error-log-table.html
//! https://github.com/mysql/mysql-server/blob/8.4/storage/perfschema/cursor_by_error_log.cc

use super::{integer, invalid, text};
use crate::{connect::Flavor, read::QueryRead, rows::Rec};
use culprit_core::{Result, telemetry::MysqlErrorLogObservation};
use mysql_async::Value;

pub(crate) mod context;

pub(crate) const MAX_ROWS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Available,
    UnsupportedProduct,
    UnsupportedVersion,
    InstrumentationDisabled,
}

pub(crate) fn availability(flavor: Flavor, version: &str, instrumentation: bool) -> Availability {
    if flavor == Flavor::Mariadb {
        Availability::UnsupportedProduct
    } else if !culprit_core::version_at_least(version, [8, 0, 22]) {
        Availability::UnsupportedVersion
    } else if !instrumentation {
        Availability::InstrumentationDisabled
    } else {
        Availability::Available
    }
}

pub(crate) struct Observations {
    pub rows: Vec<MysqlErrorLogObservation>,
    pub truncated: bool,
}

#[derive(Clone, Copy)]
enum Field {
    Logged,
    Thread,
    Priority,
    ErrorCode,
    Subsystem,
}

impl Field {
    const ALL: [Self; 5] = [Self::Logged, Self::Thread, Self::Priority, Self::ErrorCode, Self::Subsystem];

    fn name(self) -> &'static str {
        match self {
            Self::Logged => "logged_microseconds",
            Self::Thread => "thread_id",
            Self::Priority => "priority",
            Self::ErrorCode => "error_code",
            Self::Subsystem => "subsystem",
        }
    }

    fn projection(self) -> String {
        let native = match self {
            // A TIMESTAMP column is read as its internal epoch directly, without an ambiguous local-time parse.
            // https://dev.mysql.com/doc/refman/8.4/en/date-and-time-functions.html#function_unix-timestamp
            Self::Logged => "CAST(UNIX_TIMESTAMP(LOGGED) * 1000000 AS UNSIGNED)",
            Self::Thread => "THREAD_ID",
            Self::Priority => "PRIO",
            Self::ErrorCode => "ERROR_CODE",
            Self::Subsystem => "SUBSYSTEM",
        };
        format!("{native} AS {}", self.name())
    }
}

pub(crate) fn query() -> String {
    let fields = Field::ALL.map(Field::projection).join(", ");
    format!("SELECT {fields} FROM performance_schema.error_log LIMIT ?")
}

pub(crate) async fn read<R: QueryRead>(reader: &mut R, limit: usize) -> Result<Observations> {
    if limit == 0 || limit > MAX_ROWS {
        return Err(culprit_core::telemetry::CollectionStop::InvalidPage.error());
    }
    let mut rows = reader.read(&query(), vec![Value::UInt((limit + 1) as u64)], limit + 1).await?;
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    let rows = rows.iter().enumerate().map(|(position, row)| decode(row, position as u32)).collect::<Result<_>>()?;
    Ok(Observations { rows, truncated })
}

pub(crate) fn decode(row: &Rec, position: u32) -> Result<MysqlErrorLogObservation> {
    let observation = MysqlErrorLogObservation {
        position,
        logged_at: MysqlErrorLogObservation::logged_at_from_microseconds(integer(row, Field::Logged.name())?)?,
        thread_id: row.unsigned(Field::Thread.name())?.map(|value| value.to_string()),
        priority: text(row, Field::Priority.name())?,
        error_code: row.native_text(Field::ErrorCode.name())?,
        subsystem: row.native_text(Field::Subsystem.name())?,
        message: None,
    };
    if !observation.is_valid() {
        return Err(invalid());
    }
    Ok(observation)
}

#[cfg(test)]
pub(crate) mod tests;
