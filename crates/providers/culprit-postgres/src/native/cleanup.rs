//! One bounded cleanup reader for scans and capture. Native horizons and session generations never collapse.
use super::*;
use culprit_core::{CleanupHolder, MeasurementScope, TransactionActivity, telemetry::*};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Session,
    Slot,
    Prepared,
}
impl Kind {
    pub(crate) const ALL: [Self; 3] = [Self::Session, Self::Slot, Self::Prepared];
    pub(crate) fn source(self) -> &'static str {
        match self {
            Self::Session => "pg_stat_activity",
            Self::Slot => "pg_replication_slots",
            Self::Prepared => "pg_prepared_xacts",
        }
    }
}

fn branch(kind: Kind, version: u32) -> String {
    let observed = iso("pg_catalog.clock_timestamp()");
    let sql = match kind {
        Kind::Session => {
            let query_id = if version >= 140_000 { "query_id::text" } else { "NULL::text" };
            format!(
                "SELECT pid::text AS native_id,pid::text AS name,{observed} AS observed_at,
                {} AS backend_start,{} AS transaction_start,{} AS query_start,
                datid::text AS database_oid,datname AS database,
                CASE WHEN datid IS NULL THEN 'server' ELSE 'database' END AS native_scope,
                backend_xmin::text AS data_xmin,age(backend_xmin)::text AS data_xmin_age,
                backend_xid::text AS transaction_id,age(backend_xid)::text AS transaction_id_age,
                extract(epoch FROM pg_catalog.clock_timestamp()-xact_start)::text AS seconds,
                application_name AS program,usename AS username,usesysid::text AS user_oid,pg_catalog.host(client_addr) AS host,state,
                {query_id} AS query_id
                FROM pg_catalog.pg_stat_activity a
                WHERE (backend_xmin IS NOT NULL OR backend_xid IS NOT NULL)
                AND (datname=pg_catalog.current_database() OR (datname IS NULL AND backend_type='walsender'))
                AND backend_type<>'autovacuum worker' AND pid<>pg_catalog.pg_backend_pid()
                AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_progress_vacuum v WHERE v.pid=a.pid)
                AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_replication_slots logical_slot
                    WHERE logical_slot.active_pid=a.pid AND logical_slot.slot_type='logical')",
                iso("backend_start"), iso("xact_start"), iso("query_start")
            )
        }
        Kind::Slot => format!(
            "SELECT s.slot_name::text AS native_id,s.slot_name::text AS name,{observed} AS observed_at,
            'server'::text AS native_scope,s.database,s.datoid::text AS database_oid,
            s.active,s.active_pid::text AS active_pid,{} AS backend_start,
            s.xmin::text AS data_xmin,age(s.xmin)::text AS data_xmin_age,
            s.catalog_xmin::text AS catalog_xmin,age(s.catalog_xmin)::text AS catalog_xmin_age,
            s.slot_type,r.application_name AS program,r.usename AS username,pg_catalog.host(r.client_addr) AS host
            FROM pg_catalog.pg_replication_slots s
            LEFT JOIN pg_catalog.pg_stat_replication r ON r.pid=s.active_pid
            WHERE s.xmin IS NOT NULL OR s.catalog_xmin IS NOT NULL",
            iso("r.backend_start")
        ),
        Kind::Prepared => format!(
            "SELECT transaction::text AS native_id,transaction::text AS name,{observed} AS observed_at,
            'database'::text AS native_scope,database,
            (SELECT oid::text FROM pg_catalog.pg_database WHERE datname=pg_catalog.current_database()) AS database_oid,
            transaction::text AS transaction_id,age(transaction)::text AS transaction_id_age,
            {} AS prepared_at,extract(epoch FROM pg_catalog.clock_timestamp()-prepared)::text AS seconds,
            owner AS username
            FROM pg_catalog.pg_prepared_xacts WHERE database=pg_catalog.current_database()",
            iso("prepared")
        ),
    };
    format!(
        "SELECT '{}'::text AS native_source,pg_catalog.to_jsonb(s)::text AS native_values FROM ({sql}) s",
        kind.source()
    )
}

pub(crate) fn query(kind: Kind, version: u32, limit: usize) -> String {
    format!("{} LIMIT {limit}", branch(kind, version))
}
pub(crate) fn sources() -> Vec<String> {
    let mut sources = native_sources();
    sources.push(RetentionCoverage::SOURCE.into());
    sources
}
fn native_sources() -> Vec<String> {
    Kind::ALL.map(|kind| kind.source().to_owned()).to_vec()
}
pub(crate) fn coverage(standby: bool) -> RetentionCoverage {
    RetentionCoverage {
        native_sources: native_sources(),
        qualified_horizons: if standby { vec![] } else { vec![CleanupHorizon::Data, CleanupHorizon::Catalog] },
    }
}
/// Recovery's KnownAssignedXids horizon is not exposed by the database-filtered session view.
/// Slot and prepared-transaction observations remain readable; their presence cannot fill that missing boundary.
pub(crate) fn scope_complete(source: &str, standby: bool) -> bool {
    !standby || source != Kind::Session.source()
}
pub(crate) fn capture_query(version: u32, standby: bool, limit: usize) -> Result<String> {
    let branches = Kind::ALL.map(|kind| branch(kind, version));
    let encoded = serde_json::to_string(&coverage(standby)).map_err(|_| invalid())?.replace('\'', "''");
    Ok(format!(
        "SELECT * FROM (SELECT '{}'::text AS native_source,'{encoded}'::text AS native_values UNION ALL {}) cleanup_holders LIMIT {limit}",
        RetentionCoverage::SOURCE,
        branches.join(" UNION ALL ")
    ))
}
pub(crate) fn decode_coverage(row: &Row, standby: bool) -> Result<RetentionCoverage> {
    if text(row, "native_source")? != RetentionCoverage::SOURCE {
        return Err(invalid());
    }
    let decoded: RetentionCoverage = serde_json::from_str(&text(row, "native_values")?).map_err(|_| invalid())?;
    if decoded != coverage(standby) || !decoded.is_valid() {
        return Err(invalid());
    }
    Ok(decoded)
}

pub(crate) struct Reading {
    pub holders: Vec<RetentionObservation>,
    pub truncated: bool,
}
pub(crate) async fn read<R: QueryRead>(reader: &mut R, kind: Kind, version: u32, limit: usize) -> Result<Reading> {
    let limit = limit.min(MAX_RETENTION_HOLDERS);
    let rows = reader.query(&query(kind, version, limit + 1), limit + 1).await?;
    let truncated = rows.len() > limit;
    let mut identities = BTreeSet::new();
    let mut holders = Vec::new();
    for row in rows.into_iter().take(limit) {
        let holder = decode(&row)?;
        if holder.source != kind.source() || !identities.insert(holder.key()?) {
            return Err(invalid());
        }
        holders.push(holder);
    }
    Ok(Reading { holders, truncated })
}

fn optional(values: &BTreeMap<String, Value>, name: &str) -> Result<Option<String>> {
    match values.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(invalid()),
    }
}
fn required(values: &BTreeMap<String, Value>, name: &str) -> Result<String> {
    optional(values, name)?.filter(|value| !value.is_empty()).ok_or_else(invalid)
}
fn time(values: &BTreeMap<String, Value>, name: &str) -> Result<Option<String>> {
    optional(values, name)?
        .map(|at| {
            canonical_time(&at)?;
            // Match the shared activity reader's exact, microsecond UTC native generation spelling.
            Ok(at)
        })
        .transpose()
}
fn horizons(values: &BTreeMap<String, Value>, holder: CleanupHolder) -> Result<Vec<RetentionHorizon>> {
    let mut horizons = Vec::new();
    for (kind, value, age, native_name) in [
        (
            CleanupHorizon::Data,
            "data_xmin",
            "data_xmin_age",
            if holder == CleanupHolder::Session { "backend_xmin" } else { "xmin" },
        ),
        (
            CleanupHorizon::Data,
            "transaction_id",
            "transaction_id_age",
            if holder == CleanupHolder::Session { "backend_xid" } else { "transaction" },
        ),
        (CleanupHorizon::Catalog, "catalog_xmin", "catalog_xmin_age", "catalog_xmin"),
    ] {
        if let Some(native_value) = optional(values, value)? {
            native_value.parse::<u32>().map_err(|_| invalid())?;
            let transactions_behind = optional(values, age)?;
            if let Some(age) = &transactions_behind {
                age.parse::<u64>().map_err(|_| invalid())?;
            }
            horizons.push(RetentionHorizon {
                protects: match kind {
                    CleanupHorizon::Data => {
                        vec![CleanupHorizon::Data, CleanupHorizon::Catalog, CleanupHorizon::SharedCatalog]
                    }
                    CleanupHorizon::Catalog => vec![CleanupHorizon::Catalog, CleanupHorizon::SharedCatalog],
                    CleanupHorizon::SharedCatalog => return Err(invalid()),
                },
                native_name: native_name.into(),
                native_value,
                transactions_behind,
            });
        } else if optional(values, age)?.is_some() {
            return Err(invalid());
        }
    }
    Ok(horizons)
}
fn session(
    values: &BTreeMap<String, Value>,
    holder: CleanupHolder,
    native_id: &str,
) -> Result<Option<NativeActivityIdentity>> {
    let id =
        if holder == CleanupHolder::Session { Some(native_id.to_owned()) } else { optional(values, "active_pid")? };
    match (id, time(values, "backend_start")?) {
        (Some(id), Some(generation)) => {
            id.parse::<u32>().map_err(|_| invalid())?;
            Ok(Some(NativeActivityIdentity { id, generation }))
        }
        _ if holder == CleanupHolder::Session => Err(invalid()),
        _ => Ok(None),
    }
}
fn transaction(values: &BTreeMap<String, Value>) -> Result<Option<RetentionTransactionContext>> {
    let native_id = optional(values, "transaction_id")?;
    let started_at = time(values, "transaction_start")?;
    let prepared_at = time(values, "prepared_at")?;
    Ok((native_id.is_some() || started_at.is_some() || prepared_at.is_some()).then_some(RetentionTransactionContext {
        native_id,
        started_at,
        prepared_at,
    }))
}
fn query_context(values: &BTreeMap<String, Value>, state: Option<&str>) -> Result<Option<RetentionQueryContext>> {
    let started_at = time(values, "query_start")?;
    let native_id = optional(values, "query_id")?;
    if let Some(query) = &native_id {
        query.parse::<i64>().map_err(|_| invalid())?;
    }
    let identity = native_id
        .map(|query_id| {
            let mut qualifiers = BTreeMap::new();
            for name in ["database_oid", "user_oid"] {
                if let Some(value) = optional(values, name)? {
                    qualifiers.insert(name.into(), value);
                }
            }
            Ok::<_, culprit_core::Error>(NativeQueryIdentity { query_id, qualifiers })
        })
        .transpose()?;
    Ok((started_at.is_some() || identity.is_some()).then_some(RetentionQueryContext {
        identity,
        started_at,
        relationship: if state == Some("active") {
            RetentionQueryRelationship::ExecutingAtObservation
        } else {
            RetentionQueryRelationship::PreviousStatement
        },
    }))
}
fn activity(
    holder: CleanupHolder,
    values: &BTreeMap<String, Value>,
    state: Option<&str>,
) -> Result<TransactionActivity> {
    Ok(match holder {
        CleanupHolder::ReplicationSlot => match values.get("active").and_then(Value::as_bool) {
            Some(true) => TransactionActivity::SlotActive,
            Some(false) => TransactionActivity::SlotInactive,
            None => return Err(invalid()),
        },
        CleanupHolder::PreparedTransaction => TransactionActivity::Prepared,
        CleanupHolder::Session if matches!(state, Some("idle in transaction" | "idle in transaction (aborted)")) => {
            TransactionActivity::IdleInTransaction
        }
        CleanupHolder::Session => TransactionActivity::Reported,
    })
}
pub(crate) fn decode(row: &Row) -> Result<RetentionObservation> {
    let source = text(row, "native_source")?;
    let holder = match source.as_str() {
        "pg_stat_activity" => CleanupHolder::Session,
        "pg_replication_slots" => CleanupHolder::ReplicationSlot,
        "pg_prepared_xacts" => CleanupHolder::PreparedTransaction,
        _ => return Err(invalid()),
    };
    let values: BTreeMap<String, Value> = serde_json::from_str(&text(row, "native_values")?).map_err(|_| invalid())?;
    let native_id = required(&values, "native_id")?;
    let session = session(&values, holder, &native_id)?;
    let unresolved_session_id = if session.is_none() { optional(&values, "active_pid")? } else { None };
    let transaction = transaction(&values)?;
    let state = optional(&values, "state")?;
    let scope = match required(&values, "native_scope")?.as_str() {
        "database" => MeasurementScope::Database,
        "server" => MeasurementScope::Server,
        _ => return Err(invalid()),
    };
    let mut identity = NativeIdentity::new(&source).qualified("native_id", native_id);
    if scope == MeasurementScope::Database {
        identity = identity.qualified("database_oid", required(&values, "database_oid")?);
    }
    if let Some(session) = &session {
        identity = identity.qualified("backend_start", &session.generation);
    }
    if let Some(transaction) = &transaction {
        if let Some(started) = &transaction.started_at {
            identity = identity.qualified("transaction_start", started);
        }
        if let Some(prepared) = &transaction.prepared_at {
            identity = identity.qualified("prepared_at", prepared);
        }
    }
    let client = ActivityClient {
        application: optional(&values, "program")?.filter(|value| !value.is_empty()),
        user: optional(&values, "username")?,
        host: optional(&values, "host")?,
    };
    let age_seconds = optional(&values, "seconds")?.map(|age| age.parse::<f64>().map_err(|_| invalid())).transpose()?;
    let observation = RetentionObservation {
        source,
        identity,
        observed_at: canonical_time(&required(&values, "observed_at")?)?,
        scope,
        holder,
        name: required(&values, "name")?,
        horizons: horizons(&values, holder)?,
        session,
        unresolved_session_id,
        transaction,
        query: query_context(&values, state.as_deref())?,
        client: Some(client),
        activity: activity(holder, &values, state.as_deref())?,
        native_state: state,
        age_seconds,
        database: optional(&values, "database")?,
        code_ownership: PayloadUnavailable::NotCollected,
    };
    if !observation.is_valid() {
        return Err(invalid());
    }
    Ok(observation)
}

#[cfg(test)]
mod tests;
