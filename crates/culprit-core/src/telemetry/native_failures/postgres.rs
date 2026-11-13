//! Supplied PostgreSQL structured records retain native conditions and local context, not query ownership.
use super::*;

pub const MAX_POSTGRES_DEADLOCK_EDGES: usize = 128;
const MAX_TIMESTAMP_BYTES: usize = 64;
const MAX_LABEL_BYTES: usize = 1024;
const MAX_STATEMENT_BYTES: usize = 64 * 1024;
const MAX_GRAPH_BYTES: usize = 256 * 1024;

/// The condition reported by SQLSTATE. A native error condition does not identify the memory/resource consumer.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PostgresFailureKind {
    AllocationFailure,
    Deadlock,
    QueryCanceled,
    InsufficientResources,
    DiskFull,
    TooManyConnections,
    ConfigurationLimit,
    ProgramLimit,
    LockNotAvailable,
    AdminShutdown,
    CrashShutdown,
    CannotConnectNow,
    DatabaseDropped,
    IdleSessionTimeout,
    IdleTransactionTimeout,
    TransactionTimeout,
    Unclassified,
}
impl PostgresFailureKind {
    pub fn from_sql_state(code: &str) -> Self {
        match code {
            "53200" => Self::AllocationFailure,
            "40P01" => Self::Deadlock,
            "57014" => Self::QueryCanceled,
            "53000" => Self::InsufficientResources,
            "53100" => Self::DiskFull,
            "53300" => Self::TooManyConnections,
            "53400" => Self::ConfigurationLimit,
            "54000" | "54001" | "54011" | "54023" => Self::ProgramLimit,
            "55P03" => Self::LockNotAvailable,
            "57P01" => Self::AdminShutdown,
            "57P02" => Self::CrashShutdown,
            "57P03" => Self::CannotConnectNow,
            "57P04" => Self::DatabaseDropped,
            "57P05" => Self::IdleSessionTimeout,
            "25P03" => Self::IdleTransactionTimeout,
            "25P04" => Self::TransactionTimeout,
            _ => Self::Unclassified,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PostgresLogFailureObservation {
    /// Position in supplied capture, not a native occurrence identity across feeds or captures.
    pub position: u32,
    pub timestamp: String,
    /// Only a native explicit UTC/GMT or numeric offset qualifies the raw event time.
    pub timestamp_utc: Option<String>,
    pub sql_state: String,
    pub severity: Option<String>,
    /// Affected database context; this never establishes the hosting resource's database owner.
    pub database: Option<String>,
    pub process_id: Option<String>,
    /// PostgreSQL's documented quasi-unique session context. Neither field proves an ActivityKey generation.
    pub session_id: Option<String>,
    pub session_start: Option<String>,
    /// CSV and JSON have independent counters; require qualified feed stream/format/incarnation before matching.
    pub session_line_number: Option<String>,
    pub virtual_transaction_id: Option<String>,
    pub transaction_id: Option<String>,
    /// Native writers use signed int64 representation; preserve it without a floating-point conversion.
    pub query_id: Option<String>,
    pub leader_process_id: Option<String>,
    pub backend_type: Option<String>,
    pub statement: DefinitionPayload,
    pub deadlock: Option<PostgresLogDeadlock>,
}
impl PostgresLogFailureObservation {
    /// Unknown/named time zones remain unqualified. Do not reinterpret a local clock as UTC.
    pub fn qualified_timestamp(value: &str) -> Option<String> {
        if value.len() > MAX_TIMESTAMP_BYTES {
            return None;
        }
        let candidate = if let Some(utc) = value.strip_suffix(" UTC").or_else(|| value.strip_suffix(" GMT")) {
            format!("{}Z", utc.replacen(' ', "T", 1))
        } else {
            value.replacen(' ', "T", 1)
        };
        let canonical = canonical_time(&candidate).ok()?;
        // Chrono represents leap seconds using nanoseconds >= 1e9. PostgreSQL does not implement leap seconds;
        // preserve that supplied clock as unqualified native text rather than granting it an exact UTC instant.
        // https://www.postgresql.org/docs/current/datatype-datetime.html#DATATYPE-TIMEZONES
        DateTime::parse_from_rfc3339(&canonical)
            .ok()
            .filter(|instant| instant.timestamp_subsec_nanos() < 1_000_000_000)
            .map(|_| canonical)
    }

    pub fn kind(&self) -> PostgresFailureKind {
        PostgresFailureKind::from_sql_state(&self.sql_state)
    }

    pub fn key(&self) -> Result<String> {
        super::position_key("postgres-structured-log-position-v1", self.position)
    }

    pub fn is_valid(&self) -> bool {
        (self.position as usize) < MAX_NATIVE_FAILURE_OBSERVATIONS
            && native_text(&self.timestamp, MAX_TIMESTAMP_BYTES)
            && self.timestamp_utc == Self::qualified_timestamp(&self.timestamp)
            && valid_failure_code(&self.sql_state)
            && self.severity.as_deref().is_none_or(|value| native_text(value, 128))
            && self.database.as_deref().is_none_or(|value| native_identifier(value, MAX_LABEL_BYTES))
            && self.process_id.as_deref().is_none_or(process_id)
            && self.session_id.as_deref().is_none_or(session_id)
            && self.session_start.as_deref().is_none_or(|value| native_text(value, MAX_TIMESTAMP_BYTES))
            && self.session_line_number.as_deref().is_none_or(positive_line_number)
            && self.virtual_transaction_id.as_deref().is_none_or(virtual_transaction_id)
            && self.transaction_id.as_deref().is_none_or(transaction_id)
            && self.query_id.as_deref().is_none_or(query_id)
            && self.leader_process_id.as_deref().is_none_or(process_id)
            && self.backend_type.as_deref().is_none_or(|value| native_text(value, 128))
            && valid_statement(&self.statement)
            && self.deadlock.as_ref().is_none_or(|graph| self.valid_deadlock(graph))
    }

    fn valid_deadlock(&self, graph: &PostgresLogDeadlock) -> bool {
        self.kind() == PostgresFailureKind::Deadlock
            && graph.is_valid()
            && graph.processes.iter().all(|process| {
                process.victim == (self.process_id.as_deref() == Some(process.process_id.as_str()))
                    && process.statement.as_ref().is_none_or(|statement| {
                        process.victim
                            && matches!(&self.statement, DefinitionPayload::Available { text, .. } if text == statement)
                    })
            })
    }

    pub(crate) fn redact(&mut self) {
        redact_statement(&mut self.statement);
        if let Some(graph) = &mut self.deadlock {
            for process in &mut graph.processes {
                process.statement =
                    process.statement.as_deref().map(|sql| crate::redact::redact_sql(sql, crate::Engine::Postgres));
            }
        }
    }
}

/// Only native graph inputs are retained. IDs, translated explanations and fixes are derived on read.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PostgresLogDeadlock {
    pub processes: Vec<PostgresLogDeadlockProcess>,
    pub resources: Vec<PostgresLogDeadlockResource>,
    pub pattern: crate::DeadlockPattern,
}
impl PostgresLogDeadlock {
    pub fn is_valid(&self) -> bool {
        let participants: BTreeSet<_> = self.processes.iter().map(|process| process.process_id.as_str()).collect();
        !self.processes.is_empty()
            && self.processes.len() <= MAX_POSTGRES_DEADLOCK_EDGES
            && participants.len() == self.processes.len()
            && self.processes.iter().filter(|process| process.victim).count() <= 1
            && self.processes.iter().all(|process| {
                positive_process_id(&process.process_id)
                    && process.wait_mode.as_deref().is_none_or(postgres_log_lock_mode_is_valid)
                    && process.statement.as_ref().is_none_or(|sql| process.victim && sql.len() <= MAX_STATEMENT_BYTES)
            })
            && self.resources.len() == self.processes.len()
            && self.resources.iter().all(|resource| {
                native_text(&resource.native_id, MAX_LABEL_BYTES)
                    && native_text(&resource.kind, 128)
                    && resource.relation.as_deref().is_none_or(|value| native_identifier(value, MAX_LABEL_BYTES))
                    && participants.contains(resource.holder.as_str())
                    && participants.contains(resource.waiter.as_str())
                    && postgres_log_lock_mode_is_valid(&resource.wait_mode)
            })
            && self.processes.iter().all(|process| {
                let mut waits = self.resources.iter().filter(|resource| resource.waiter == process.process_id);
                matches!((waits.next(), waits.next()), (Some(wait), None)
                    if process.wait_mode.as_deref() == Some(wait.wait_mode.as_str()))
            })
            && postgres_deadlock_cycle_is_valid(
                self.resources.iter().map(|resource| (resource.waiter.as_str(), resource.holder.as_str())),
            )
            && self.context_bytes() <= MAX_GRAPH_BYTES
    }

    fn context_bytes(&self) -> usize {
        self.processes
            .iter()
            .map(|process| {
                process.process_id.len()
                    + process.wait_mode.as_ref().map_or(0, String::len)
                    + process.statement.as_ref().map_or(0, String::len)
            })
            .sum::<usize>()
            + self
                .resources
                .iter()
                .map(|resource| {
                    resource.native_id.len()
                        + resource.kind.len()
                        + resource.relation.as_ref().map_or(0, String::len)
                        + resource.holder.len()
                        + resource.waiter.len()
                        + resource.wait_mode.len()
                })
                .sum::<usize>()
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PostgresLogDeadlockProcess {
    pub process_id: String,
    pub victim: bool,
    pub wait_mode: Option<String>,
    /// Only the reporting backend's separate native statement is attributed; other participants remain unresolved.
    pub statement: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PostgresLogDeadlockResource {
    pub native_id: String,
    pub kind: String,
    pub relation: Option<String>,
    pub holder: String,
    pub waiter: String,
    pub wait_mode: String,
}

fn valid_failure_code(code: &str) -> bool {
    code.len() == 5
        && code.bytes().all(|value| value.is_ascii_digit() || value.is_ascii_uppercase())
        && !matches!(&code[..2], "00" | "01" | "02")
}
fn native_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}
fn native_identifier(value: &str, max_bytes: usize) -> bool {
    // PostgreSQL permits quoted identifiers containing line breaks; text-only rendering and SQL redaction are
    // separate boundaries. Do not discard a real affected database/relation merely for such a name.
    !value.is_empty() && value.len() <= max_bytes && !value.contains('\0')
}
fn process_id(value: &str) -> bool {
    value.len() <= 10 && value.parse::<i32>().is_ok_and(|number| number >= 0 && number.to_string() == value)
}
fn positive_process_id(value: &str) -> bool {
    process_id(value) && value != "0"
}
fn positive_line_number(value: &str) -> bool {
    value.len() <= 19 && value.parse::<i64>().is_ok_and(|number| number > 0 && number.to_string() == value)
}
fn transaction_id(value: &str) -> bool {
    value.len() <= 10 && value.parse::<u32>().is_ok_and(|number| number.to_string() == value)
}
fn query_id(value: &str) -> bool {
    value.len() <= 20 && value.parse::<i64>().is_ok_and(|number| number.to_string() == value)
}
fn virtual_transaction_id(value: &str) -> bool {
    value.split_once('/').is_some_and(|(backend, transaction)| transaction_id(backend) && transaction_id(transaction))
}
fn session_id(value: &str) -> bool {
    value.split_once('.').is_some_and(|(start, process)| {
        [start, process].into_iter().all(|part| {
            !part.is_empty()
                && part.len() <= 8
                && part.bytes().all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
        })
    })
}
pub fn postgres_log_lock_mode_is_valid(value: &str) -> bool {
    matches!(
        value,
        "AccessShareLock"
            | "RowShareLock"
            | "RowExclusiveLock"
            | "ShareUpdateExclusiveLock"
            | "ShareLock"
            | "ShareRowExclusiveLock"
            | "ExclusiveLock"
            | "AccessExclusiveLock"
    )
}

/// PostgreSQL's native deadlock report describes one closed cycle. A tail into another cycle, a self-loop or
/// disconnected cycles cannot qualify the reported graph, even when every PID has some edge.
pub fn postgres_deadlock_cycle_is_valid<'a>(edges: impl IntoIterator<Item = (&'a str, &'a str)>) -> bool {
    let mut waits = std::collections::BTreeMap::new();
    for (waiter, holder) in edges {
        if waits.len() == MAX_POSTGRES_DEADLOCK_EDGES
            || waiter == holder
            || !positive_process_id(waiter)
            || !positive_process_id(holder)
            || waits.insert(waiter, holder).is_some()
        {
            return false;
        }
    }
    let Some((&start, _)) = waits.first_key_value() else { return false };
    if waits.len() < 2 {
        return false;
    }
    let mut visited = BTreeSet::new();
    let mut participant = start;
    while visited.insert(participant) {
        let Some(holder) = waits.get(participant) else { return false };
        participant = holder;
    }
    participant == start && visited.len() == waits.len()
}
fn valid_statement(payload: &DefinitionPayload) -> bool {
    payload.is_valid()
        && match payload {
            DefinitionPayload::Available { text, .. } => text.len() <= MAX_STATEMENT_BYTES,
            DefinitionPayload::Unavailable { .. } => true,
        }
}
fn redact_statement(payload: &mut DefinitionPayload) {
    if let DefinitionPayload::Available { text, .. } = payload {
        *payload = DefinitionPayload::sql(text, crate::Engine::Postgres);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation() -> PostgresLogFailureObservation {
        PostgresLogFailureObservation {
            position: 0,
            timestamp: "2026-11-13 12:00:01.123 UTC".into(),
            timestamp_utc: Some("2026-11-13T12:00:01.123000000Z".into()),
            sql_state: "53200".into(),
            severity: Some("ERROR".into()),
            database: Some("app".into()),
            process_id: Some("539".into()),
            session_id: Some("6915c000.21b".into()),
            session_start: Some("2026-11-13 11:30:00 UTC".into()),
            session_line_number: Some("7".into()),
            virtual_transaction_id: Some("4/42".into()),
            transaction_id: Some("838".into()),
            query_id: Some("-9007199254740993".into()),
            leader_process_id: Some("0".into()),
            backend_type: Some("client backend".into()),
            statement: DefinitionPayload::sql(
                "SELECT 'private-token' FROM items WHERE id = 42",
                crate::Engine::Postgres,
            ),
            deadlock: None,
        }
    }

    fn deadlock() -> PostgresLogFailureObservation {
        let mut observation = observation();
        observation.sql_state = "40P01".into();
        let statement = match &observation.statement {
            DefinitionPayload::Available { text, .. } => text.clone(),
            _ => unreachable!("test statement"),
        };
        observation.deadlock = Some(PostgresLogDeadlock {
            processes: vec![
                PostgresLogDeadlockProcess {
                    process_id: "539".into(),
                    victim: true,
                    wait_mode: Some("ShareLock".into()),
                    statement: Some(statement),
                },
                PostgresLogDeadlockProcess {
                    process_id: "545".into(),
                    victim: false,
                    wait_mode: Some("ShareLock".into()),
                    statement: None,
                },
            ],
            resources: vec![
                PostgresLogDeadlockResource {
                    native_id: "transaction 838".into(),
                    kind: "transaction".into(),
                    relation: None,
                    holder: "545".into(),
                    waiter: "539".into(),
                    wait_mode: "ShareLock".into(),
                },
                PostgresLogDeadlockResource {
                    native_id: "transaction 837".into(),
                    kind: "transaction".into(),
                    relation: None,
                    holder: "539".into(),
                    waiter: "545".into(),
                    wait_mode: "ShareLock".into(),
                },
            ],
            pattern: crate::DeadlockPattern::OppositeOrder,
        });
        observation
    }

    #[test]
    fn only_explicit_native_clock_offsets_qualify_and_the_qualification_must_match_raw_time() {
        for (native, expected) in [
            ("2026-11-13 12:00:01.123 UTC", Some("2026-11-13T12:00:01.123000000Z")),
            ("2026-11-13 12:00:01.123 GMT", Some("2026-11-13T12:00:01.123000000Z")),
            ("2026-11-13T14:00:01.123456+02:00", Some("2026-11-13T12:00:01.123456000Z")),
            ("2026-11-13 12:00:01 EET", None),
            ("2026-11-13 12:00:01", None),
            ("2026-11-13T12:00:01.0000000001Z", None),
            ("2016-12-31 23:59:60 UTC", None),
        ] {
            let mut row = observation();
            row.timestamp = native.into();
            row.timestamp_utc = PostgresLogFailureObservation::qualified_timestamp(native);
            assert_eq!(row.timestamp_utc.as_deref(), expected);
            assert!(row.is_valid(), "{native}");
            row.timestamp_utc = Some("2026-11-13T12:00:02.000000000Z".into());
            assert!(!row.is_valid(), "no invented or substituted native clock");
        }
        assert!(PostgresLogFailureObservation::qualified_timestamp(&"1".repeat(65)).is_none());
    }

    #[test]
    fn malformed_native_id_widths_codes_and_missing_generations_do_not_turn_into_healthy_zeros() {
        for (field, value) in [
            ("sql_state", "40p01"),
            ("sql_state", "00000"),
            ("sql_state", "532000"),
            ("process_id", "2147483648"),
            ("process_id", "+539"),
            ("process_id", "00539"),
            ("session_id", "unknown"),
            ("session_line_number", "0"),
            ("session_line_number", "9223372036854775808"),
            ("virtual_transaction_id", "4/not-native"),
            ("transaction_id", "4294967296"),
            ("query_id", "9223372036854775808"),
        ] {
            let mut value_object = serde_json::to_value(observation()).unwrap();
            value_object[field] = serde_json::Value::String(value.into());
            let row: PostgresLogFailureObservation = serde_json::from_value(value_object).unwrap();
            assert!(!row.is_valid(), "{field} = {value}");
        }
        let mut missing = observation();
        missing.session_id = None;
        missing.session_start = None;
        missing.session_line_number = None;
        missing.database = None;
        missing.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing };
        assert!(missing.is_valid(), "missing native context is allowed; there is no fallback identity");
        assert_eq!(missing.kind(), PostgresFailureKind::AllocationFailure);
    }

    #[test]
    fn input_positions_preserve_duplicates_without_a_pid_timestamp_or_content_identity() {
        let one = observation();
        let mut two = one.clone();
        two.position = 1;
        assert_ne!(one.key().unwrap(), two.key().unwrap());
        two.position = one.position;
        two.process_id = None;
        assert_eq!(one.key().unwrap(), two.key().unwrap(), "key is capture-local position only");
        two.position = MAX_NATIVE_FAILURE_OBSERVATIONS as u32;
        assert!(two.key().is_err());
        assert!(!two.is_valid());
    }

    #[test]
    fn only_one_closed_cycle_can_qualify_native_participants() {
        assert!(postgres_deadlock_cycle_is_valid([("1", "2"), ("2", "3"), ("3", "1")]));
        for edges in [
            vec![("1", "2"), ("2", "2")],
            vec![("1", "2"), ("2", "3")],
            vec![("1", "2"), ("2", "1"), ("3", "4"), ("4", "3")],
            vec![("1", "2"), ("2", "1"), ("1", "3")],
            vec![("1", "1")],
        ] {
            assert!(!postgres_deadlock_cycle_is_valid(edges));
        }
        let mut row = deadlock();
        row.deadlock.as_mut().unwrap().resources[1].holder = "545".into();
        assert!(!row.is_valid(), "a tail entering a self-loop is not a qualified cycle");
        let mut row = deadlock();
        row.deadlock.as_mut().unwrap().resources[0].wait_mode = "FutureLock".into();
        row.deadlock.as_mut().unwrap().processes[0].wait_mode = Some("FutureLock".into());
        assert!(!row.is_valid(), "unknown native lock semantics remain unsupported graph context");
    }

    #[test]
    fn deadlock_graphs_keep_only_reported_participants_and_separately_attributed_statement() {
        assert!(deadlock().is_valid());
        let mut row = deadlock();
        row.deadlock.as_mut().unwrap().processes[1].statement = Some("SELECT 1".into());
        assert!(!row.is_valid(), "another PID has no established statement or generation");
        let mut row = deadlock();
        row.deadlock.as_mut().unwrap().resources[0].holder = "999".into();
        assert!(!row.is_valid(), "an unresolved/cut graph cannot claim a complete native cycle");
        let mut row = deadlock();
        row.deadlock.as_mut().unwrap().processes[0].wait_mode = Some("AccessExclusiveLock".into());
        assert!(!row.is_valid(), "participant and unique native wait edge must describe the same requested mode");
        let mut row = deadlock();
        row.process_id = Some("545".into());
        assert!(!row.is_valid(), "reporter attribution cannot be reassigned");
        let mut row = deadlock();
        row.sql_state = "53200".into();
        assert!(!row.is_valid(), "error text cannot give an allocation failure a deadlock graph");
        row.deadlock = None;
        assert!(row.is_valid());
    }

    #[test]
    fn native_graph_context_has_independent_count_and_cumulative_byte_limits() {
        let mut row = deadlock();
        let graph = row.deadlock.as_mut().unwrap();
        graph.processes.clear();
        graph.resources.clear();
        for i in 1..=MAX_POSTGRES_DEADLOCK_EDGES {
            let process = (1000 + i).to_string();
            graph.processes.push(PostgresLogDeadlockProcess {
                process_id: process.clone(),
                victim: false,
                wait_mode: Some("ShareLock".into()),
                statement: None,
            });
            graph.resources.push(PostgresLogDeadlockResource {
                native_id: "i".repeat(MAX_LABEL_BYTES),
                kind: "transaction".into(),
                relation: Some("r".repeat(MAX_LABEL_BYTES)),
                holder: (1000 + i % MAX_POSTGRES_DEADLOCK_EDGES + 1).to_string(),
                waiter: process,
                wait_mode: "ShareLock".into(),
            });
        }
        assert!(!row.is_valid(), "individually bounded fields still exceed cumulative graph bytes");
        for resource in &mut row.deadlock.as_mut().unwrap().resources {
            resource.relation = None;
        }
        assert!(row.is_valid(), "native count at the explicit limit remains representable");
        row.deadlock.as_mut().unwrap().processes.push(PostgresLogDeadlockProcess {
            process_id: "9999".into(),
            victim: false,
            wait_mode: None,
            statement: None,
        });
        assert!(!row.is_valid());
    }

    #[test]
    fn export_redacts_sql_without_free_form_messages_or_client_labels_and_accepts_native_quoted_names() {
        let mut row = deadlock();
        let raw = "SELECT 'private-token' FROM items WHERE id = 42";
        row.statement = DefinitionPayload::Available {
            text: raw.into(),
            sha256: digest(raw.as_bytes()),
            redaction_version: REDACTION_VERSION,
        };
        row.deadlock.as_mut().unwrap().processes[0].statement = Some(raw.into());
        assert!(row.is_valid());
        row.redact();
        assert!(row.is_valid());
        let once = row.clone();
        row.redact();
        assert_eq!(row, once, "privacy boundary is idempotent");
        let json = serde_json::to_string(&row).unwrap();
        assert!(!json.contains("private-token"));
        let object = serde_json::to_value(&row).unwrap();
        for absent in ["message", "detail", "context", "client", "connection_from", "remote_port"] {
            assert!(object.get(absent).is_none(), "{absent} is not an export field");
        }
        row.database = Some("tenant\napp".into());
        row.deadlock.as_mut().unwrap().resources[0].relation = Some("table\nname".into());
        assert!(row.is_valid(), "quoted native identifiers can contain line breaks without becoming code owners");
    }
}
