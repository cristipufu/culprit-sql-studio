//! One native activity reader for Live and persistent-issue capture. Lock-manager work is bounded and conditional.
use super::metadata::Metadata;
use super::*;
use culprit_core::{Engine, Session, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};
const MAX_BLOCKING_LOOKUPS: usize = 64;
const MAX_BLOCKING_DEPTH: usize = 8;
pub struct ActivityRecord {
    pub facts: Option<ActivityObservation>,
    row: Row,
    sql: Option<String>,
}
impl ActivityRecord {
    pub fn native_text(&self, name: &str) -> Option<String> {
        self.row.text(name)
    }
}
pub struct ActivityFrame {
    pub at: String,
    pub records: Vec<ActivityRecord>,
    pub locks: Vec<culprit_core::LockWait>,
}
impl ActivityFrame {
    pub fn observations(&self) -> Vec<ActivityObservation> {
        self.records.iter().filter_map(|row| row.facts.clone()).collect()
    }
    pub(crate) fn fully_visible(&self, meta: &Metadata) -> bool {
        meta.full_visibility && self.fully_visible_for(&meta.database_oid)
    }
    pub fn fully_visible_for(&self, database_oid: &str) -> bool {
        self.records
            .iter()
            .filter(|row| row.row.text("database_oid").as_deref() == Some(database_oid))
            .all(|row| row.facts.is_some() && row.row.text("state").as_deref() != Some("disabled"))
    }
}
fn age(start: &str) -> String {
    format!(
        "CASE WHEN {start} IS NOT NULL AND stamp.at>={start} THEN floor(extract(epoch FROM stamp.at-{start})*1000)::text END"
    )
}
pub(crate) fn query(version: u32, limit: usize) -> String {
    query_layout(version >= 140_000, "'client backend'", "", limit)
}
pub fn query_layout(query_id: bool, backend_types: &str, extra_columns: &str, limit: usize) -> String {
    let query_id = if query_id { "a.query_id::text" } else { "NULL::text" };
    format!(
        "WITH stamp AS MATERIALIZED (SELECT pg_catalog.clock_timestamp() AS at)
        SELECT {} AS observed_at,a.pid::text AS pid,{} AS backend_start,{} AS query_start,{} AS transaction_start,
        a.datid::text AS database_oid,a.datname AS database,a.usename AS username,a.application_name,
        coalesce(a.client_hostname,pg_catalog.host(a.client_addr)) AS host,coalesce(a.state,a.backend_type) AS state,
        a.wait_event_type,a.wait_event,{} AS elapsed_ms,{} AS transaction_ms,{} AS idle_ms,
        {query_id} AS query_id{extra_columns}
        FROM stamp LEFT JOIN LATERAL (SELECT * FROM pg_catalog.pg_stat_activity
            WHERE pid<>pg_catalog.pg_backend_pid() AND backend_type IN ({backend_types}) LIMIT {limit}) a ON true",
        iso("stamp.at"),
        iso("a.backend_start"),
        iso("a.query_start"),
        iso("a.xact_start"),
        age("a.query_start"),
        age("a.xact_start"),
        age("a.state_change")
    )
}
fn optional_counter(row: &Row, name: &str) -> Result<Option<String>> {
    row.text(name).map(|raw| raw.parse::<u64>().map(|value| value.to_string()).map_err(|_| invalid())).transpose()
}
fn decode(row: Row, database_oid: &str) -> Result<ActivityRecord> {
    let pid = text(&row, "pid")?;
    pid.parse::<u32>().map_err(|_| invalid())?;
    let state = text(&row, "state")?;
    let active = state == "active";
    let scope = if row.text("database_oid").as_deref() == Some(database_oid) {
        ActivityScopeBasis::SessionDatabase
    } else {
        ActivityScopeBasis::OtherDatabase
    };
    let backend_start = row.text("backend_start");
    let query_start = row.text("query_start");
    let facts = if let Some(backend_start) = backend_start.filter(|_| !active || query_start.is_some()) {
        let request = if active {
            Some(NativeActivityIdentity {
                id: "statement".into(),
                generation: query_start.clone().ok_or_else(invalid)?,
            })
        } else {
            None
        };
        let transaction = row.text("transaction_start");
        let native_type = match (row.text("wait_event_type"), row.text("wait_event")) {
            (Some(kind), Some(event)) => Some(format!("{kind}: {event}")),
            (Some(kind), None) => Some(kind),
            _ => None,
        };
        let observation = ActivityObservation {
            blocking_status: culprit_core::telemetry::ActivityBlockingStatus::Known,
            key: ActivityKey {
                session: NativeActivityIdentity { id: pid, generation: backend_start },
                request,
                transaction: if active { None } else { transaction.clone() },
            },
            idle: !active,
            scope,
            state,
            database: row.text("database"),
            command: None,
            last_request_age_ms: if active { None } else { optional_counter(&row, "elapsed_ms")? },
            open_transactions: u32::from(transaction.is_some()),
            elapsed_ms: if active { optional_counter(&row, "elapsed_ms")? } else { None },
            transaction_age_ms: optional_counter(&row, "transaction_ms")?,
            session_counters: ActivityCounters::default(),
            request_counters: active.then(ActivityCounters::default),
            wait: native_type.map(|native_type| ActivityWait {
                resources: None,
                native_type: Some(native_type),
                elapsed_ms: None,
                owners: vec![],
            }),
            query_hash: row.text("query_id"),
            client: Some(ActivityClient {
                application: row.text("application_name"),
                user: row.text("username"),
                host: row.text("host"),
            }),
            statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        };
        if !observation.is_valid() {
            return Err(invalid());
        }
        Some(observation)
    } else {
        None
    };
    Ok(ActivityRecord { facts, row, sql: None })
}
pub(crate) async fn observe<R: QueryRead>(
    reader: &mut R,
    meta: &Metadata,
    budget: &CollectionBudget,
) -> Result<ActivityFrame> {
    if meta.version < 120_000 {
        return Err(culprit_core::Error::new(
            culprit_core::ErrorCode::Unsupported,
            culprit_core::t!("collection.unsupported"),
        ));
    }
    observe_rows(reader, &query(meta.version, budget.max_snapshot_rows as usize + 1), &meta.database_oid, budget).await
}
pub async fn observe_rows<R: QueryRead>(
    reader: &mut R,
    sql: &str,
    database_oid: &str,
    budget: &CollectionBudget,
) -> Result<ActivityFrame> {
    let rows = reader.query(sql, budget.max_snapshot_rows as usize + 1).await?;
    let at = text(rows.first().ok_or_else(invalid)?, "observed_at")?;
    let mut records = vec![];
    for row in rows {
        if text(&row, "observed_at")? != at {
            return Err(invalid());
        }
        if row.text("pid").is_some() {
            records.push(decode(row, database_oid)?);
        }
    }
    if records.len() > budget.max_snapshot_rows as usize {
        return Err(CollectionStop::RowLimit.error());
    }
    ActivityEvidence::validate(&records.iter().filter_map(|row| row.facts.clone()).collect::<Vec<_>>())?;
    Ok(ActivityFrame { at, records, locks: vec![] })
}
fn selector(record: &ActivityRecord) -> Result<String> {
    let facts = record.facts.as_ref().ok_or_else(invalid)?;
    let pid = facts.key.session.id.parse::<u32>().map_err(|_| invalid())?;
    let started = canonical_time(&facts.key.session.generation)?;
    let query_start = record
        .row
        .text("query_start")
        .map(|value| canonical_time(&value).map(|value| format!("'{value}'::timestamptz")))
        .transpose()?
        .unwrap_or_else(|| "NULL::timestamptz".into());
    Ok(format!("({pid},'{started}'::timestamptz,{query_start})"))
}
#[derive(Clone, Copy)]
pub(crate) enum ActivityScope {
    Visible,
    CurrentDatabase,
}
pub(crate) struct BlockingReadOptions {
    pub minimum_request_age_ms: u32,
    pub scope: ActivityScope,
    pub wait_start_available: bool,
}
pub(crate) async fn blockers<R: QueryRead>(
    reader: &mut R,
    frame: &mut ActivityFrame,
    options: BlockingReadOptions,
) -> Result<()> {
    let by_pid = frame
        .records
        .iter()
        .enumerate()
        .map(|(index, row)| Ok((text(&row.row, "pid")?.parse::<u32>().map_err(|_| invalid())?, index)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut pending = frame
        .records
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            row.row.text("wait_event_type").as_deref() == Some("Lock")
                && row.facts.as_ref().is_some_and(|facts| {
                    (matches!(options.scope, ActivityScope::Visible)
                        || facts.scope != ActivityScopeBasis::OtherDatabase)
                        && facts
                            .elapsed_ms
                            .as_ref()
                            .and_then(|value| value.parse::<u64>().ok())
                            .is_some_and(|value| value >= options.minimum_request_age_ms.into())
                })
        })
        .map(|(index, _)| index)
        .collect::<BTreeSet<_>>();
    let mut visited = BTreeSet::new();
    for _ in 0..MAX_BLOCKING_DEPTH {
        if pending.is_empty() {
            break;
        }
        if pending.len() + visited.len() > MAX_BLOCKING_LOOKUPS {
            return Err(CollectionStop::RowLimit.error());
        }
        let targets =
            pending.iter().map(|index| selector(&frame.records[*index])).collect::<Result<Vec<_>>>()?.join(",");
        let sql = format!(
            "WITH targets(pid,backend_start,query_start) AS (VALUES {targets}) SELECT a.pid::text AS pid,pg_catalog.array_to_json(pg_catalog.pg_blocking_pids(a.pid))::text AS owners FROM targets t JOIN pg_catalog.pg_stat_activity a ON a.pid=t.pid AND a.backend_start=t.backend_start AND a.query_start IS NOT DISTINCT FROM t.query_start WHERE a.wait_event_type='Lock'"
        );
        let rows = reader.query(&sql, pending.len()).await?;
        visited.append(&mut pending);
        for row in rows {
            let pid = integer(&row, "pid")? as u32;
            let index = *by_pid.get(&pid).ok_or_else(invalid)?;
            let owners: Vec<u32> = serde_json::from_str(&text(&row, "owners")?).map_err(|_| invalid())?;
            if owners.len() > 64 {
                return Err(CollectionStop::RowLimit.error());
            }
            let mut waits = vec![];
            for owner in owners {
                if owner == 0 {
                    waits.push(ActivityWaitOwner::Transaction {
                        native_id: "0".into(),
                        owner: TransactionWaitOwner::Prepared,
                    });
                    continue;
                }
                let record = by_pid.get(&owner).map(|index| &frame.records[*index]);
                let generation =
                    record.and_then(|row| row.facts.as_ref()).map(|row| row.key.session.generation.clone());
                waits.push(ActivityWaitOwner::Session { id: owner.to_string(), generation });
                if let Some(owner_index) = by_pid.get(&owner).filter(|index| !visited.contains(index))
                    && frame.records[*owner_index].row.text("wait_event_type").as_deref() == Some("Lock")
                    && frame.records[*owner_index].facts.is_some()
                {
                    pending.insert(*owner_index);
                }
            }
            if let Some(wait) = frame.records[index].facts.as_mut().and_then(|facts| facts.wait.as_mut()) {
                wait.owners = waits;
            }
        }
    }
    if !pending.is_empty() {
        return Err(CollectionStop::RowLimit.error());
    }
    if visited.is_empty() || !options.wait_start_available {
        return Ok(());
    }
    let lock_targets =
        visited.iter().map(|index| selector(&frame.records[*index])).collect::<Result<Vec<_>>>()?.join(",");
    // One global lock snapshot per troubled poll, never one scan per waiter. Healthy polls do not read pg_locks.
    let sql=format!("WITH targets(pid,backend_start,query_start) AS (VALUES {lock_targets}),
        current_waiters AS MATERIALIZED (SELECT a.pid FROM targets t JOIN pg_catalog.pg_stat_activity a ON a.pid=t.pid AND a.backend_start=t.backend_start AND a.query_start IS NOT DISTINCT FROM t.query_start WHERE a.wait_event_type='Lock'),
        waits AS MATERIALIZED (SELECT w.pid,w.waitstart,w.mode,w.locktype,w.relation,w.database FROM pg_catalog.pg_locks w WHERE NOT w.granted AND w.pid IN (SELECT pid FROM current_waiters)),
        stamp AS (SELECT pg_catalog.clock_timestamp() AS at)
        SELECT w.pid::text AS pid,CASE WHEN stamp.at>=w.waitstart THEN floor(extract(epoch FROM stamp.at-w.waitstart)*1000)::text END AS elapsed_ms,
        w.mode,w.locktype,n.nspname||'.'||o.relname AS object,CASE WHEN c.relkind IN ('i','I') THEN c.relname END AS index_name
        FROM waits w CROSS JOIN stamp
        LEFT JOIN pg_catalog.pg_class c ON c.oid=w.relation AND (w.database=0 OR w.database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=pg_catalog.current_database()))
        LEFT JOIN pg_catalog.pg_index ix ON ix.indexrelid=c.oid LEFT JOIN pg_catalog.pg_class o ON o.oid=coalesce(ix.indrelid,c.oid)
        LEFT JOIN pg_catalog.pg_namespace n ON n.oid=o.relnamespace");
    for index in &visited {
        if let Some(wait) = frame.records[*index].facts.as_mut().and_then(|facts| facts.wait.as_mut()) {
            wait.resources = Some(vec![]);
        }
    }
    for row in reader.query(&sql, visited.len()).await? {
        let pid = integer(&row, "pid")? as u32;
        let index = *by_pid.get(&pid).ok_or_else(invalid)?;
        let resource = culprit_core::LockWait {
            session: pid.to_string(),
            mode: row.text("mode"),
            resource: row.text("locktype"),
            object: row.text("object"),
            index: row.text("index_name"),
        };
        if let Some(wait) = frame.records[index].facts.as_mut().and_then(|facts| facts.wait.as_mut()) {
            wait.elapsed_ms = optional_counter(&row, "elapsed_ms")?;
            wait.resources.as_mut().ok_or_else(invalid)?.push(resource.clone());
        }
        frame.locks.push(resource);
    }
    Ok(())
}
pub(crate) async fn enrich<R: QueryRead>(
    reader: &mut R,
    frame: &mut ActivityFrame,
    selected: &BTreeSet<ActivityKey>,
    max_sql_bytes: usize,
    include_idle: bool,
) -> Result<ActivityDetailStatus> {
    enrich_for(reader, frame, selected, max_sql_bytes, include_idle, Engine::Postgres).await
}
pub async fn enrich_for<R: QueryRead>(
    reader: &mut R,
    frame: &mut ActivityFrame,
    selected: &BTreeSet<ActivityKey>,
    max_sql_bytes: usize,
    include_idle: bool,
    engine: Engine,
) -> Result<ActivityDetailStatus> {
    let targets = frame
        .records
        .iter()
        .filter(|row| {
            row.facts
                .as_ref()
                .is_some_and(|facts| selected.contains(&facts.key) && (include_idle || facts.key.request.is_some()))
        })
        .map(selector)
        .collect::<Result<Vec<_>>>()?;
    if targets.is_empty() {
        return Ok(ActivityDetailStatus::NotNeeded);
    }
    let sql = format!(
        "WITH targets(pid,backend_start,query_start) AS (VALUES {}) SELECT a.pid::text AS pid,CASE WHEN pg_catalog.octet_length(a.query)<={max_sql_bytes} THEN a.query ELSE NULL END AS query,pg_catalog.octet_length(a.query)::text AS query_bytes FROM targets t JOIN pg_catalog.pg_stat_activity a ON a.pid=t.pid AND a.backend_start=t.backend_start AND a.query_start IS NOT DISTINCT FROM t.query_start",
        targets.join(",")
    );
    let rows = reader.query(&sql, targets.len()).await?;
    let by_pid = rows.into_iter().map(|row| Ok((integer(&row, "pid")?, row))).collect::<Result<BTreeMap<_, _>>>()?;
    for record in &mut frame.records {
        let Some(facts) = record.facts.as_mut().filter(|facts| selected.contains(&facts.key)) else {
            continue;
        };
        let pid = facts.key.session.id.parse::<u64>().map_err(|_| invalid())?;
        if let Some(row) = by_pid.get(&pid) {
            record.sql = row.text("query");
            if facts.key.request.is_some() {
                facts.statement = if let Some(sql) = &record.sql {
                    DefinitionPayload::sql(sql, engine)
                } else {
                    DefinitionPayload::Unavailable {
                        reason: if integer(row, "query_bytes").is_ok_and(|bytes| bytes > max_sql_bytes as u64) {
                            PayloadUnavailable::Oversized
                        } else {
                            PayloadUnavailable::Missing
                        },
                    }
                };
            }
        }
    }
    Ok(ActivityDetailStatus::Collected)
}
pub(crate) fn project(record: &ActivityRecord) -> Session {
    let row = &record.row;
    let active = row.text("state").as_deref() == Some("active");
    let wait = match (row.text("wait_event_type"), row.text("wait_event")) {
        (Some(kind), Some(event)) => Some(format!("{kind}: {event}")),
        (Some(kind), None) => Some(kind),
        _ => None,
    };
    Session {
        id: row.string("pid"),
        user: row.text("username"),
        application: row.text("application_name").filter(|value| !value.is_empty()),
        host: row.text("host"),
        database: row.text("database"),
        command: if active {
            record.sql.as_deref().and_then(crate::workload::leading_keyword).map(|value| value.to_uppercase())
        } else {
            None
        },
        wait: if active { wait } else { None },
        waiting_on_lock: Some(active && row.text("wait_event_type").as_deref() == Some("Lock")),
        wait_ms: record
            .facts
            .as_ref()
            .and_then(|facts| facts.wait.as_ref())
            .and_then(|wait| wait.elapsed_ms.as_ref())
            .and_then(|value| value.parse().ok()),
        blocked_by: record
            .facts
            .as_ref()
            .and_then(|facts| facts.wait.as_ref())
            .and_then(|wait| wait.owners.first())
            .and_then(|owner| match owner {
                ActivityWaitOwner::Session { id, .. } => Some(id.clone()),
                _ => None,
            }),
        elapsed_ms: row.number(if active { "elapsed_ms" } else { "idle_ms" }),
        transaction_ms: row.number("transaction_ms"),
        cpu_ms: None,
        reads: None,
        sql: record.sql.clone(),
        state: row.string("state"),
    }
}
