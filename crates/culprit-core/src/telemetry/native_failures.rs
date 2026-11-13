//! Native error records retain their source vocabulary. A captured row is neither a current pressure assessment
//! nor a globally unique incident; connection numbers without a generation never establish a query owner.
use super::*;
use std::collections::BTreeSet;

mod postgres;
pub use postgres::*;

pub const MAX_NATIVE_FAILURE_OBSERVATIONS: usize = 4096;
const MAX_NATIVE_MESSAGE_BYTES: usize = 65_535;

/// The error-log buffer exposes connection identifiers, not Performance Schema thread identifiers.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MysqlErrorLogObservation {
    pub position: u32,
    /// A qualified UTC instant from the native TIMESTAMP value, with at most microsecond precision.
    pub logged_at: String,
    /// Exact unsigned decimal; zero, SQL NULL and a missing generation remain distinct.
    pub thread_id: Option<String>,
    pub priority: String,
    pub error_code: Option<String>,
    pub subsystem: Option<String>,
    /// Free-form messages are omitted by default. Every telemetry export clears them before persistence.
    pub message: Option<String>,
}

impl MysqlErrorLogObservation {
    /// MySQL LOGGED is TIMESTAMP(6), whose range is narrower than UNIX_TIMESTAMP's argument range.
    pub fn logged_at_from_microseconds(microseconds: u64) -> Result<String> {
        let seconds = microseconds / 1_000_000;
        if !(1..=i32::MAX as u64).contains(&seconds) {
            return Err(CollectionStop::InvalidPage.error());
        }
        DateTime::<Utc>::from_timestamp(seconds as i64, ((microseconds % 1_000_000) * 1000) as u32)
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
            .ok_or_else(|| CollectionStop::InvalidPage.error())
    }

    pub fn key(&self) -> Result<String> {
        position_key("mysql-error-log-position-v1", self.position)
    }

    pub fn is_valid(&self) -> bool {
        (self.position as usize) < MAX_NATIVE_FAILURE_OBSERVATIONS
            && self.logged_at.len() <= 64
            && DateTime::parse_from_rfc3339(&self.logged_at).is_ok_and(|at| {
                at.offset().local_minus_utc() == 0
                    && (1..=i64::from(i32::MAX)).contains(&at.timestamp())
                    && at.timestamp_subsec_nanos() < 1_000_000_000
                    && at.timestamp_subsec_nanos() % 1000 == 0
            })
            && self.thread_id.as_deref().is_none_or(unsigned_decimal)
            && bounded_label(&self.priority, 32)
            && self.error_code.as_deref().is_none_or(|code| bounded_label(code, 10))
            && self.subsystem.as_deref().is_none_or(|subsystem| bounded_label(subsystem, 7))
            && self.message.as_ref().is_none_or(|message| message.len() <= MAX_NATIVE_MESSAGE_BYTES)
    }

    pub(crate) fn redact(&mut self) {
        self.message = None;
    }
}

/// A native-format discriminator selects native interpretation, not a guessed database engine or text pattern.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "native", content = "observation", rename_all = "kebab-case")]
#[ts(export)]
pub enum NativeFailureRecord {
    MysqlErrorLog(MysqlErrorLogObservation),
    PostgresLog(Box<PostgresLogFailureObservation>),
}

impl NativeFailureRecord {
    pub fn position(&self) -> u32 {
        match self {
            Self::MysqlErrorLog(row) => row.position,
            Self::PostgresLog(row) => row.position,
        }
    }

    fn engine(&self) -> crate::Engine {
        match self {
            Self::MysqlErrorLog(_) => crate::Engine::Mysql,
            Self::PostgresLog(_) => crate::Engine::Postgres,
        }
    }

    fn is_valid(&self) -> bool {
        match self {
            Self::MysqlErrorLog(row) => row.is_valid(),
            Self::PostgresLog(row) => row.is_valid(),
        }
    }

    pub(crate) fn redact(&mut self) {
        match self {
            Self::MysqlErrorLog(row) => row.redact(),
            Self::PostgresLog(row) => row.redact(),
        }
    }
}

/// The read bounds qualify capture, not native event times or historical completeness. These are hosting-level
/// diagnostics: a native connection ID or database name does not establish a member database's resource ownership.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct NativeFailureSnapshot {
    pub source: String,
    pub observed_from: String,
    pub observed_until: String,
    pub resource: Option<SharedResourceIdentity>,
    pub observations: Vec<NativeFailureRecord>,
}

impl NativeFailureSnapshot {
    pub fn is_valid(&self) -> bool {
        let mut positions = BTreeSet::new();
        let engine = self.observations.first().map(NativeFailureRecord::engine);
        valid_key(&self.source)
            && self.observed_from.len() <= 64
            && self.observed_until.len() <= 64
            && matches!(
                (DateTime::parse_from_rfc3339(&self.observed_from), DateTime::parse_from_rfc3339(&self.observed_until)),
                (Ok(from), Ok(until)) if from <= until
                    && from.offset().local_minus_utc() == 0 && until.offset().local_minus_utc() == 0
            )
            && self.resource.as_ref().is_none_or(|resource| {
                resource.is_valid()
                    && resource.kind == SharedResourceKind::Instance
                    && engine.is_none_or(|engine| resource.engine == engine)
            })
            && self.observations.len() <= MAX_NATIVE_FAILURE_OBSERVATIONS
            && self
                .observations
                .iter()
                .all(|row| row.is_valid() && Some(row.engine()) == engine && positions.insert(row.position()))
    }

    pub fn redact(&mut self) {
        for row in &mut self.observations {
            row.redact();
        }
    }
}

fn position_key(native: &str, position: u32) -> Result<String> {
    if position as usize >= MAX_NATIVE_FAILURE_OBSERVATIONS {
        return Err(CollectionStop::InvalidPage.error());
    }
    serde_json::to_vec(&(native, position)).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
}

fn unsigned_decimal(value: &str) -> bool {
    value.len() <= 20 && value.parse::<u64>().is_ok_and(|number| number.to_string() == value)
}

fn bounded_label(value: &str, max_characters: usize) -> bool {
    value.len() <= max_characters * 4
        && value.chars().take(max_characters + 1).count() <= max_characters
        && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests;
