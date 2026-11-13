//! Instrumented session allocations, independently of any statement that happens to be current.
//!
//! These native indexes iterate the thread container; they are not constant-time ID lookups. Read each bounded,
//! unsorted projection once and match native identities in memory, rather than issuing a lookup per thread or a
//! join that can multiply native iteration. A row cap bounds returned evidence, not sparse-container work or full
//! visibility. Production-load qualification remains separate from this explicitly requested diagnostic profile.
//! https://dev.mysql.com/doc/refman/8.4/en/performance-schema-threads-table.html
//! https://github.com/mysql/mysql-server/blob/8.4/storage/perfschema/cursor_by_thread.cc
//! https://github.com/mysql/mysql-server/blob/8.4/storage/perfschema/table_events_statements.cc
use super::{instance, integer, invalid, one, text};
use crate::{connect::Flavor, read::QueryRead, rows::Rec};
use culprit_core::{Result, telemetry::*};
use mysql_async::Value;
use std::collections::{BTreeMap, BTreeSet};

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
    } else if !culprit_core::version_at_least(version, [8, 0, 31]) {
        Availability::UnsupportedVersion
    } else if !instrumentation {
        Availability::InstrumentationDisabled
    } else {
        Availability::Available
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Consumers {
    pub global: bool,
    pub thread: bool,
    pub statements: bool,
}

impl Consumers {
    pub(crate) fn memory_enabled(self) -> bool {
        self.global && self.thread
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Context {
    pub instance: instance::Identity,
    pub consumers: Consumers,
}

pub(crate) async fn context<R: QueryRead>(reader: &mut R) -> Result<(Context, String)> {
    let platform = instance::platform(reader).await?;
    let support = availability(platform.flavor, &platform.version, platform.performance_schema);
    if support != Availability::Available {
        return Err(unavailable(support));
    }
    let configuration = one(reader, "SELECT @@GLOBAL.server_uuid AS server_uuid", vec![]).await?;
    let instance = platform.identify(reader, &configuration).await?;
    let consumers = consumers(reader).await?;
    Ok((Context { instance, consumers }, platform.observed_at))
}

pub(crate) fn unavailable(support: Availability) -> culprit_core::Error {
    let message = match support {
        Availability::Available => return invalid(),
        Availability::InstrumentationDisabled => {
            culprit_core::message!("provider.mysql.session_memory.instrumentation")
        }
        Availability::UnsupportedProduct => culprit_core::message!("provider.mysql.session_memory.mariadb"),
        Availability::UnsupportedVersion => culprit_core::message!("provider.mysql.session_memory.version"),
    };
    culprit_core::Error::new(culprit_core::ErrorCode::Unsupported, message)
}

async fn consumers<R: QueryRead>(reader: &mut R) -> Result<Consumers> {
    let rows = reader
        .read(
            "SELECT NAME AS name, ENABLED AS enabled FROM performance_schema.setup_consumers
             WHERE NAME IN ('global_instrumentation','thread_instrumentation','events_statements_current')",
            vec![],
            4,
        )
        .await?;
    let mut values = BTreeMap::new();
    for row in &rows {
        if values.insert(text(row, "name")?, enabled(row, "enabled")?).is_some() {
            return Err(invalid());
        }
    }
    if values.len() != 3 {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    Ok(Consumers {
        global: *values.get("global_instrumentation").ok_or_else(invalid)?,
        thread: *values.get("thread_instrumentation").ok_or_else(invalid)?,
        statements: *values.get("events_statements_current").ok_or_else(invalid)?,
    })
}

pub(crate) struct Session {
    pub session: NativeActivityIdentity,
    pub observed_at: String,
    pub instrumented: bool,
    pub current_bytes: Option<String>,
    pub controlled_current_bytes: Option<String>,
    pub high_water_bytes: Option<String>,
    pub controlled_high_water_bytes: Option<String>,
}

pub(crate) struct CurrentEvent {
    pub observed_at: String,
    pub request: NativeActivityIdentity,
    pub database: Option<String>,
    pub query_hash: Option<String>,
}

pub(crate) struct Observations {
    pub sessions: Vec<Session>,
    pub current: BTreeMap<String, CurrentEvent>,
    pub truncated: bool,
    pub contexts_available: bool,
    pub context_failure: Option<culprit_core::Error>,
}

#[derive(Clone, Copy)]
enum ThreadField {
    Thread,
    Connection,
    Type,
    Instrumented,
    Current,
    ControlledCurrent,
    HighWater,
    ControlledHighWater,
    ObservedAt,
}

impl ThreadField {
    const ALL: [Self; 9] = [
        Self::Thread,
        Self::Connection,
        Self::Type,
        Self::Instrumented,
        Self::Current,
        Self::ControlledCurrent,
        Self::HighWater,
        Self::ControlledHighWater,
        Self::ObservedAt,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Thread => "thread_id",
            Self::Connection => "connection_id",
            Self::Type => "thread_type",
            Self::Instrumented => "instrumented",
            Self::Current => "current_bytes",
            Self::ControlledCurrent => "controlled_current_bytes",
            Self::HighWater => "high_water_bytes",
            Self::ControlledHighWater => "controlled_high_water_bytes",
            Self::ObservedAt => "observed_at",
        }
    }

    fn projection(self) -> String {
        let native = match self {
            Self::Thread => "THREAD_ID",
            Self::Connection => "PROCESSLIST_ID",
            Self::Type => "TYPE",
            Self::Instrumented => "INSTRUMENTED",
            Self::Current => "TOTAL_MEMORY",
            Self::ControlledCurrent => "CONTROLLED_MEMORY",
            Self::HighWater => "MAX_TOTAL_MEMORY",
            Self::ControlledHighWater => "MAX_CONTROLLED_MEMORY",
            Self::ObservedAt => "DATE_FORMAT(UTC_TIMESTAMP(6), '%Y-%m-%dT%H:%i:%s.%fZ')",
        };
        format!("{native} AS {}", self.name())
    }
}

pub(crate) fn thread_query() -> String {
    format!(
        "SELECT {} FROM performance_schema.threads LIMIT ?",
        ThreadField::ALL.map(ThreadField::projection).join(", ")
    )
}

pub(crate) fn event_query() -> &'static str {
    "SELECT THREAD_ID AS thread_id, EVENT_ID AS event_id, END_EVENT_ID AS end_event_id,
        TIMER_START AS timer_start, CURRENT_SCHEMA AS database_name, DIGEST AS query_hash,
        NESTING_EVENT_TYPE AS nesting_type,
        DATE_FORMAT(UTC_TIMESTAMP(6), '%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at
        FROM performance_schema.events_statements_current LIMIT ?"
}

pub(crate) async fn read<R: QueryRead>(reader: &mut R, context: &Context, limit: usize) -> Result<Observations> {
    if limit == 0 || limit > MAX_ROWS {
        return Err(invalid());
    }
    let mut rows = reader.read(&thread_query(), vec![Value::UInt((limit + 1) as u64)], limit + 1).await?;
    let mut truncated = rows.len() > limit;
    rows.truncate(limit);
    let mut sessions = Vec::new();
    let mut seen = BTreeSet::new();
    for row in &rows {
        let thread = integer(row, ThreadField::Thread.name())?;
        if thread == 0 || !seen.insert(thread) {
            return Err(invalid());
        }
        if let Some(session) = session(row, context)? {
            sessions.push(session);
        }
    }
    let mut contexts_available = context.consumers.memory_enabled() && context.consumers.statements;
    let mut context_failure = None;
    let current = if contexts_available && sessions.iter().any(|row| row.instrumented) {
        let events = reader.read(event_query(), vec![Value::UInt((limit + 1) as u64)], limit + 1).await;
        let mut events = match events {
            Ok(events) => events,
            Err(error) if error.code == culprit_core::ErrorCode::PermissionDenied && error.detail.is_none() => {
                contexts_available = false;
                context_failure = Some(error);
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        truncated |= events.len() > limit;
        events.truncate(limit);
        current_events(&events, &sessions)?
    } else {
        BTreeMap::new()
    };
    Ok(Observations { sessions, current, truncated, contexts_available, context_failure })
}

fn session(row: &Rec, context: &Context) -> Result<Option<Session>> {
    let thread_type = text(row, ThreadField::Type.name())?;
    if !matches!(thread_type.as_str(), "FOREGROUND" | "BACKGROUND") {
        return Err(invalid());
    }
    let Some(connection) = row.unsigned(ThreadField::Connection.name())? else {
        return Ok(None);
    };
    // Only this connection's numeric ID is qualified on the current native hosting instance. A cancellation
    // connection reached through the same route can be hosted elsewhere; its number cannot exclude a local row.
    if thread_type != "FOREGROUND" || connection == context.instance.connection_id {
        return Ok(None);
    }
    if connection == 0 {
        return Err(invalid());
    }
    let instrumented = enabled(row, ThreadField::Instrumented.name())?;
    let measured = instrumented && context.consumers.memory_enabled();
    let value = |field: ThreadField| -> Result<Option<String>> {
        let native = row.unsigned(field.name())?;
        Ok(if measured { native.map(|value| value.to_string()) } else { None })
    };
    Ok(Some(Session {
        session: NativeActivityIdentity {
            id: connection.to_string(),
            generation: integer(row, ThreadField::Thread.name())?.to_string(),
        },
        observed_at: text(row, ThreadField::ObservedAt.name())?,
        instrumented,
        current_bytes: value(ThreadField::Current)?,
        controlled_current_bytes: value(ThreadField::ControlledCurrent)?,
        high_water_bytes: value(ThreadField::HighWater)?,
        controlled_high_water_bytes: value(ThreadField::ControlledHighWater)?,
    }))
}

fn current_events(rows: &[Rec], sessions: &[Session]) -> Result<BTreeMap<String, CurrentEvent>> {
    let eligible: BTreeSet<_> =
        sessions.iter().filter(|row| row.instrumented).map(|row| row.session.generation.clone()).collect();
    let mut seen = BTreeSet::new();
    let mut current = BTreeMap::new();
    for row in rows {
        let thread = integer(row, "thread_id")?.to_string();
        let event = integer(row, "event_id")?;
        if event == 0 || !seen.insert((thread.clone(), event)) {
            return Err(invalid());
        }
        if !eligible.contains(&thread) || row.unsigned("end_event_id")?.is_some() {
            continue;
        }
        // Only a native top-level event can supply this context. Its inner statements remain unresolved rather
        // than choosing an arbitrary stack row or claiming which statement acquired the session allocations.
        if !matches!(row.native_text("nesting_type")?.as_deref(), None | Some("TRANSACTION")) {
            continue;
        }
        let Some(start) = row.unsigned("timer_start")? else {
            continue;
        };
        let event = CurrentEvent {
            observed_at: text(row, "observed_at")?,
            request: NativeActivityIdentity { id: event.to_string(), generation: start.to_string() },
            database: row.native_text("database_name")?,
            query_hash: row.native_text("query_hash")?,
        };
        if current.insert(thread, event).is_some() {
            return Err(invalid());
        }
    }
    Ok(current)
}

fn enabled(row: &Rec, name: &str) -> Result<bool> {
    match text(row, name)?.as_str() {
        "YES" => Ok(true),
        "NO" => Ok(false),
        _ => Err(invalid()),
    }
}

#[cfg(test)]
pub(crate) mod tests;
