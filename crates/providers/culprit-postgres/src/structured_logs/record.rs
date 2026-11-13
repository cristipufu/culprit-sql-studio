use super::NativeLogProblem;
use culprit_core::Deadlock;
pub use culprit_core::telemetry::PostgresFailureKind;
use culprit_core::telemetry::{
    ActivityClient, CollectionStop, DefinitionPayload, PayloadUnavailable, PostgresLogDeadlock,
    PostgresLogDeadlockProcess, PostgresLogDeadlockResource, PostgresLogFailureObservation,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) const CSV_FIELDS: [&str; 26] = [
    "timestamp",
    "user",
    "dbname",
    "pid",
    "connection_from",
    "session_id",
    "line_num",
    "ps",
    "session_start",
    "vxid",
    "txid",
    "error_severity",
    "state_code",
    "message",
    "detail",
    "hint",
    "internal_query",
    "internal_position",
    "context",
    "statement",
    "cursor_position",
    "location",
    "application_name",
    "backend_type",
    "leader_pid",
    "query_id",
];

pub(super) fn csv_fields(format: super::LogFormat) -> &'static [&'static str] {
    if format == super::LogFormat::Csv13 { &CSV_FIELDS[..24] } else { &CSV_FIELDS }
}

pub(super) fn known_field(name: &str) -> bool {
    CSV_FIELDS.contains(&name) || matches!(name, "remote_host" | "remote_port")
}

/// PostgreSQL documents session_id as quasi-unique and formats session_start only to seconds. This is native
/// context, not an exact backend generation or occurrence identity. Each native log format has its own line counter;
/// even candidate matching needs independently qualified stream/format/incarnation before comparing this tuple.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeSessionLine<'a> {
    pub session_id: &'a str,
    pub session_start: &'a str,
    pub line_number: &'a str,
}

/// Native/private parser output. Arbitrary message/detail/context and client labels must not be exported unchanged;
/// the ingestion adapter owns privacy policy and uses DefinitionPayload::sql for retained SQL.
#[derive(Clone, Debug, PartialEq)]
pub struct PostgresLogRecord {
    pub position: u32,
    pub timestamp: String,
    pub timestamp_utc: Option<String>,
    pub sql_state: String,
    pub kind: PostgresFailureKind,
    pub severity: Option<String>,
    pub database: Option<String>,
    pub process_id: Option<String>,
    pub session_id: Option<String>,
    pub session_start: Option<String>,
    pub session_line_number: Option<String>,
    pub virtual_transaction_id: Option<String>,
    pub transaction_id: Option<String>,
    pub query_id: Option<String>,
    pub leader_process_id: Option<String>,
    pub backend_type: Option<String>,
    pub client: ActivityClient,
    /// CSV supplies the native client host:port field as one opaque value; it is not parsed as a host identity.
    pub connection_from: Option<String>,
    pub remote_port: Option<String>,
    pub message: Option<String>,
    pub detail: Option<String>,
    pub context: Option<String>,
    pub statement: Option<String>,
    /// Missing or unparsed native DETAIL retains the deadlock condition without inventing graph participants.
    pub deadlock: Option<Deadlock>,
}
impl PostgresLogRecord {
    pub fn native_session_line(&self) -> Option<NativeSessionLine<'_>> {
        Some(NativeSessionLine {
            session_id: self.session_id.as_deref()?,
            session_start: self.session_start.as_deref()?,
            line_number: self.session_line_number.as_deref()?,
        })
    }

    /// Consume private parser output into the safe export contract. Free-form diagnostics and client labels are
    /// deliberately omitted, and retained SQL uses the same redaction boundary as ordinary collected definitions.
    pub fn into_observation(self) -> culprit_core::Result<PostgresLogFailureObservation> {
        let mut observation = PostgresLogFailureObservation {
            position: self.position,
            timestamp: self.timestamp,
            timestamp_utc: self.timestamp_utc,
            sql_state: self.sql_state,
            severity: self.severity,
            database: self.database,
            process_id: self.process_id,
            session_id: self.session_id,
            session_start: self.session_start,
            session_line_number: self.session_line_number,
            virtual_transaction_id: self.virtual_transaction_id,
            transaction_id: self.transaction_id,
            query_id: self.query_id,
            leader_process_id: self.leader_process_id,
            backend_type: self.backend_type,
            statement: statement_payload(self.statement.as_deref()),
            deadlock: None,
        };
        if !observation.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        observation.deadlock = self.deadlock.and_then(|graph| safe_graph(graph).ok());
        if !observation.is_valid() {
            // Graph qualification is separate from the positive native SQLSTATE. Unsupported DETAIL must never
            // discard a valid failure condition, its native context or its separately redacted statement.
            observation.deadlock = None;
        }
        Ok(observation)
    }
}

fn statement_payload(statement: Option<&str>) -> DefinitionPayload {
    statement.map_or(DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing }, |statement| {
        DefinitionPayload::sql(statement, culprit_core::Engine::Postgres)
    })
}

fn safe_graph(graph: Deadlock) -> culprit_core::Result<PostgresLogDeadlock> {
    let mut resources = Vec::with_capacity(graph.resources.len());
    for resource in graph.resources {
        let ([holder], [waiter]) = (resource.holders.as_slice(), resource.waiters.as_slice()) else {
            return Err(CollectionStop::InvalidPage.error());
        };
        resources.push(PostgresLogDeadlockResource {
            native_id: resource.id,
            kind: resource.kind,
            relation: resource.object,
            holder: holder.process.clone(),
            waiter: waiter.process.clone(),
            wait_mode: waiter.mode.clone().ok_or_else(|| CollectionStop::InvalidPage.error())?,
        });
    }
    Ok(PostgresLogDeadlock {
        processes: graph
            .processes
            .into_iter()
            .map(|process| PostgresLogDeadlockProcess {
                process_id: process.id,
                victim: process.victim,
                wait_mode: process.wait_mode,
                statement: process
                    .statement
                    .as_deref()
                    .map(|sql| culprit_core::redact::redact_sql(sql, culprit_core::Engine::Postgres)),
            })
            .collect(),
        resources,
        pattern: graph.pattern,
    })
}

type Fields = BTreeMap<String, Value>;

#[derive(Clone, Copy)]
enum IntegerKind {
    Process,
    Line,
    Transaction,
    Query,
    Port,
}
impl IntegerKind {
    fn accepts(self, value: &str) -> bool {
        match self {
            Self::Process => value.parse::<i32>().is_ok_and(|value| value >= 0),
            Self::Line => value.parse::<i64>().is_ok_and(|value| value > 0),
            Self::Transaction => value.parse::<u32>().is_ok(),
            Self::Query => value.parse::<i64>().is_ok(),
            Self::Port => value.parse::<u16>().is_ok(),
        }
    }
}

fn optional_text(fields: &Fields, key: &'static str) -> std::result::Result<Option<String>, NativeLogProblem> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(NativeLogProblem::InvalidField(key.into())),
    }
}

fn integer(
    fields: &Fields,
    key: &'static str,
    kind: IntegerKind,
) -> std::result::Result<Option<String>, NativeLogProblem> {
    let value = match fields.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(value)) if value.is_empty() => return Ok(None),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) if value.is_u64() || value.is_i64() => value.to_string(),
        _ => return Err(NativeLogProblem::InvalidField(key.into())),
    };
    if value.len() <= 20 && kind.accepts(&value) {
        Ok(Some(value))
    } else {
        Err(NativeLogProblem::InvalidField(key.into()))
    }
}

pub(super) fn decode(fields: Fields) -> std::result::Result<Option<PostgresLogRecord>, NativeLogProblem> {
    let sql_state =
        optional_text(&fields, "state_code")?.ok_or_else(|| NativeLogProblem::InvalidField("state_code".into()))?;
    if sql_state.len() != 5 || !sql_state.bytes().all(|value| value.is_ascii_digit() || value.is_ascii_uppercase()) {
        return Err(NativeLogProblem::InvalidField("state_code".into()));
    }
    if matches!(&sql_state[..2], "00" | "01" | "02") {
        return Ok(None);
    }
    let timestamp =
        optional_text(&fields, "timestamp")?.ok_or_else(|| NativeLogProblem::InvalidField("timestamp".into()))?;
    let kind = PostgresFailureKind::from_sql_state(&sql_state);
    let process_id = integer(&fields, "pid", IntegerKind::Process)?;
    let detail = optional_text(&fields, "detail")?;
    let context = optional_text(&fields, "context")?;
    let statement = optional_text(&fields, "statement")?;
    let deadlock = if kind == PostgresFailureKind::Deadlock {
        detail.as_deref().and_then(|detail| {
            crate::deadlocks::parse_deadlock_details(
                detail,
                crate::deadlocks::DeadlockLogContext {
                    context: context.as_deref(),
                    reporter: process_id.as_deref(),
                    statement: statement.as_deref(),
                    at: PostgresLogFailureObservation::qualified_timestamp(&timestamp),
                },
            )
        })
    } else {
        None
    };
    Ok(Some(PostgresLogRecord {
        position: 0,
        timestamp_utc: PostgresLogFailureObservation::qualified_timestamp(&timestamp),
        timestamp,
        sql_state,
        kind,
        severity: optional_text(&fields, "error_severity")?,
        database: optional_text(&fields, "dbname")?,
        process_id,
        session_id: optional_text(&fields, "session_id")?,
        session_start: optional_text(&fields, "session_start")?,
        session_line_number: integer(&fields, "line_num", IntegerKind::Line)?,
        virtual_transaction_id: optional_text(&fields, "vxid")?,
        transaction_id: integer(&fields, "txid", IntegerKind::Transaction)?,
        query_id: integer(&fields, "query_id", IntegerKind::Query)?,
        leader_process_id: integer(&fields, "leader_pid", IntegerKind::Process)?,
        backend_type: optional_text(&fields, "backend_type")?,
        client: ActivityClient {
            application: optional_text(&fields, "application_name")?,
            user: optional_text(&fields, "user")?,
            host: optional_text(&fields, "remote_host")?,
        },
        connection_from: optional_text(&fields, "connection_from")?,
        remote_port: integer(&fields, "remote_port", IntegerKind::Port)?,
        message: optional_text(&fields, "message")?,
        detail,
        context,
        statement,
        deadlock,
    }))
}
