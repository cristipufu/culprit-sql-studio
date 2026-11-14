//! Current foreground metadata only. No SQL text, transaction scan or lock-manager walk precedes baseline persistence.
use super::*;
use culprit_core::{Error, ErrorCode, telemetry::*};
use mysql_async::Value;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

pub(crate) const SOURCE: &str = "mysql-current-activity";
// Native status codes documented as waits, unlike the ambiguous System lock/User lock states.
// https://dev.mysql.com/doc/refman/8.4/en/general-thread-states.html
const LOCK_WAIT_STATES: &[&str] = &[
    "Waiting for commit lock",
    "Waiting for table level lock",
    "Waiting for event metadata lock",
    "Waiting for global read lock",
    "Waiting for schema metadata lock",
    "Waiting for stored function metadata lock",
    "Waiting for stored procedure metadata lock",
    "Waiting for table metadata lock",
    "Waiting for trigger metadata lock",
];
pub(crate) fn waiting_on_lock(state: Option<&str>, native_owner: bool) -> Option<bool> {
    (native_owner || state.is_some_and(|state| LOCK_WAIT_STATES.iter().any(|known| state.eq_ignore_ascii_case(known))))
        .then_some(true)
}
pub(crate) struct Frame {
    pub observations: Vec<ActivityObservation>,
    pub native_rows: u32,
    pub at: String,
    pub coverage: StateSourceCoverage,
}
pub(crate) async fn observe<R: QueryRead>(
    reader: &mut R,
    meta: &metadata::Metadata,
    budget: &CollectionBudget,
) -> Result<Frame> {
    if !meta.connection.performance_schema {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let consumers = reader.read(
        "SELECT NAME AS name, ENABLED AS enabled FROM performance_schema.setup_consumers WHERE NAME IN ('global_instrumentation','thread_instrumentation','events_statements_current')",
        vec![], 4,
    ).await?;
    let enabled: std::collections::BTreeSet<_> = consumers
        .iter()
        .filter(|row| row.text("enabled").as_deref() == Some("YES"))
        .filter_map(|row| row.text("name"))
        .collect();
    let instrumented = consumers.len() == 3
        && ["global_instrumentation", "thread_instrumentation", "events_statements_current"]
            .iter()
            .all(|name| enabled.contains(*name));
    let limit = budget.max_snapshot_rows as usize;
    let rows = sessions(reader, &meta.connection.database_name, limit + 1).await?;
    if rows.len() > limit + 1 {
        return Err(invalid());
    }
    let at =
        rows.first().map(|row| text(row, "observed_at")).transpose()?.unwrap_or_else(|| meta.connection.at.clone());
    let at = canonical_time(&at)?;
    let mut observations = Vec::new();
    let mut fully_represented = true;
    for row in rows.iter().take(limit) {
        if canonical_time(&text(row, "observed_at")?)? != at {
            return Err(invalid());
        }
        if let Some(observation) = decode(row, &meta.connection.database_name, instrumented)? {
            fully_represented &= observation.idle || observation.key.request.is_some();
            observations.push(observation);
        } else {
            fully_represented = false;
        }
    }
    ActivityEvidence::validate(&observations)?;
    let limited = rows.len() > limit;
    let complete = meta.process_wide && instrumented && fully_represented && !limited;
    let mut coverage = StateSourceCoverage::observed(SOURCE.into(), observations.len() as u32, complete);
    if !meta.process_wide {
        coverage.error_code = Some(ErrorCode::PermissionDenied);
    }
    if limited {
        coverage.stopped = Some(CollectionStop::RowLimit);
    } else if !complete {
        coverage.stopped = Some(CollectionStop::SourceUnavailable);
    }
    Ok(Frame { observations, native_rows: rows.len() as u32, at, coverage })
}

async fn sessions<R: QueryRead>(reader: &mut R, database: &str, limit: usize) -> Result<Vec<Rec>> {
    let sql = format!("SELECT t.PROCESSLIST_ID AS id, t.THREAD_ID AS thread_id, t.PROCESSLIST_DB AS db,
        t.PROCESSLIST_USER AS user_name, t.PROCESSLIST_HOST AS host, t.PROCESSLIST_COMMAND AS command,
        t.PROCESSLIST_STATE AS state, t.PROCESSLIST_TIME AS seconds, t.INSTRUMENTED AS instrumented,
        e.EVENT_ID AS event_id, e.TIMER_START AS timer_start, e.TIMER_WAIT AS elapsed_ps, e.END_EVENT_ID AS end_event_id,
        e.CURRENT_SCHEMA AS request_db, e.DIGEST AS query_hash,
        DATE_FORMAT(UTC_TIMESTAMP(6), '%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at
        FROM performance_schema.threads t LEFT JOIN performance_schema.events_statements_current e ON e.THREAD_ID=t.THREAD_ID
        WHERE t.TYPE='FOREGROUND' AND t.PROCESSLIST_ID<>CONNECTION_ID() AND t.PROCESSLIST_DB=?
          AND t.PROCESSLIST_COMMAND NOT IN ('Daemon','Binlog Dump','Binlog Dump GTID') LIMIT {limit}");
    reader.read(&sql, vec![Value::from(database)], limit).await
}
fn decode(row: &Rec, database: &str, instrumentation_enabled: bool) -> Result<Option<ActivityObservation>> {
    let Some(command) = row.native_text("command")?.filter(|value| !value.is_empty()) else { return Ok(None) };
    let Some(session_id) = row.unsigned("id")? else { return Ok(None) };
    let Some(thread_id) = row.unsigned("thread_id")? else { return Ok(None) };
    let idle = command == "Sleep";
    let elapsed = row.unsigned("elapsed_ps")?;
    let event = row.unsigned("event_id")?;
    let started = row.unsigned("timer_start")?;
    let request = if !idle
        && instrumentation_enabled
        && row.native_text("instrumented")?.as_deref() == Some("YES")
        && row.unsigned("end_event_id")?.is_none()
        && elapsed.is_some()
    {
        event
            .zip(started)
            .map(|(id, generation)| NativeActivityIdentity { id: id.to_string(), generation: generation.to_string() })
    } else {
        None
    };
    let statement_identity = request.as_ref().map(|request| {
        NativeIdentity::new(digest(
            format!("{session_id}|{thread_id}|{}|{}", request.id, request.generation).as_bytes(),
        ))
        .qualified("source", "performance_schema.events_statements_current")
        .qualified("basis", "thread-event-start")
    });
    let state = row.native_text("state")?.filter(|value| !value.is_empty()).unwrap_or_else(|| command.clone());
    let lock_wait = waiting_on_lock(Some(&state), false);
    let wait_elapsed = if lock_wait.is_some() {
        row.unsigned("seconds")?.and_then(|seconds| seconds.checked_mul(1000)).map(|age| age.to_string())
    } else {
        None
    };
    let request_db = row.native_text("request_db")?;
    let observation = ActivityObservation {
        waiting_on_lock: request.as_ref().and(lock_wait),
        blocking_status: ActivityBlockingStatus::Unknown,
        key: ActivityKey {
            session: NativeActivityIdentity { id: session_id.to_string(), generation: thread_id.to_string() },
            request: request.clone(),
            transaction: None,
        },
        idle,
        scope: if request.is_some() && request_db.as_deref() == Some(database) {
            ActivityScopeBasis::RequestDatabase
        } else {
            ActivityScopeBasis::SessionDatabase
        },
        state: state.clone(),
        database: if request.is_some() { request_db.or(row.native_text("db")?) } else { row.native_text("db")? },
        command: Some(command),
        last_request_age_ms: None,
        open_transactions: None,
        elapsed_ms: if request.is_some() { elapsed.map(|value| (value / 1_000_000_000).to_string()) } else { None },
        transaction_age_ms: None,
        session_counters: Default::default(),
        request_counters: request.is_some().then(ActivityCounters::default),
        wait: lock_wait.map(|_| ActivityWait {
            identified_resources: vec![],
            native_type: Some(state),
            elapsed_ms: wait_elapsed,
            owners: vec![],
            resources: None,
        }),
        query_hash: row.native_text("query_hash")?,
        statement_identity,
        client: Some(ActivityClient {
            application: None,
            user: row.native_text("user_name")?,
            host: row.native_text("host")?,
        }),
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
    };
    if !observation.is_valid() {
        return Err(invalid());
    }
    Ok(Some(observation))
}
