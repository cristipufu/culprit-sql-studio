//! Cleanup horizon holders share one native reader with the scan. Current values never imply reset continuity.
use super::*;
use culprit_core::{CleanupHolder, MeasurementScope, OpenTransaction, TransactionActivity, telemetry::*};
use std::collections::BTreeMap;
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Session,
    Slot,
    Prepared,
}
impl Kind {
    pub fn source(self) -> &'static str {
        match self {
            Self::Session => "pg_stat_activity",
            Self::Slot => "pg_replication_slots",
            Self::Prepared => "pg_prepared_xacts",
        }
    }
}
pub(crate) fn query(kind: Kind, limit: Option<usize>) -> String {
    let sql = match kind {
        Kind::Session => format!(
            "SELECT pid::text AS native_id, {} AS native_start,
            CASE WHEN datname IS NULL THEN 'server' ELSE 'database' END AS native_scope,
            pid::text AS session,extract(epoch FROM now()-coalesce(xact_start,backend_start)) AS seconds,
            application_name AS program,state,greatest(age(backend_xmin),age(backend_xid)) AS xid_age
            FROM pg_catalog.pg_stat_activity WHERE (backend_xmin IS NOT NULL OR backend_xid IS NOT NULL)
            AND (datname=pg_catalog.current_database() OR (datname IS NULL AND backend_type='walsender'))
            AND backend_type<>'autovacuum worker' AND pid<>pg_catalog.pg_backend_pid()",
            iso("coalesce(xact_start,backend_start)")
        ),
        Kind::Slot => "SELECT slot_name::text AS native_id,NULL::text AS native_start,'server'::text AS native_scope,
            slot_name AS session,active,greatest(age(xmin),age(catalog_xmin)) AS xid_age,
            (xmin IS NULL OR (catalog_xmin IS NOT NULL AND age(catalog_xmin)>age(xmin))) AS catalog_only
            FROM pg_catalog.pg_replication_slots WHERE xmin IS NOT NULL OR catalog_xmin IS NOT NULL"
            .to_owned(),
        Kind::Prepared => format!(
            "SELECT transaction::text AS native_id,{} AS native_start,'database'::text AS native_scope,
            gid AS session,extract(epoch FROM now()-prepared) AS seconds,owner,age(transaction) AS xid_age
            FROM pg_catalog.pg_prepared_xacts WHERE database=pg_catalog.current_database()",
            iso("prepared")
        ),
    };
    let sql = format!("{sql} ORDER BY xid_age DESC");
    match limit {
        Some(limit) => format!("{sql} LIMIT {limit}"),
        None => sql,
    }
}
pub(crate) fn sources() -> Vec<String> {
    [Kind::Session, Kind::Slot, Kind::Prepared].map(|kind| kind.source().to_owned()).to_vec()
}
pub(crate) fn capture_query(limit: usize) -> String {
    let branches=[Kind::Session,Kind::Slot,Kind::Prepared].map(|kind|{
        let session=if matches!(kind,Kind::Prepared){"pg_catalog.md5(s.session)"}else{"s.session"};
        let session_key=if matches!(kind,Kind::Prepared){"gid_md5"}else{"session"};
        format!("SELECT '{}'::text AS native_source,(pg_catalog.to_jsonb(s)-'session'||pg_catalog.jsonb_build_object('{session_key}',{session}))::text AS native_values FROM ({}) s",kind.source(),query(kind,None))
    });
    format!("SELECT * FROM ({}) cleanup_holders LIMIT {limit}", branches.join(" UNION ALL "))
}
pub(crate) fn decode(row: &Row, database_oid: &str) -> Result<NativeCatalogCounters> {
    let source = text(row, "native_source")?;
    if !sources().contains(&source) {
        return Err(invalid());
    }
    let values: BTreeMap<String, serde_json::Value> =
        serde_json::from_str(&text(row, "native_values")?).map_err(|_| invalid())?;
    let mut properties = BTreeMap::new();
    for (name, value) in values {
        if !value.is_null() {
            let value = match value {
                serde_json::Value::String(value) => value,
                other => serde_json::to_string(&other).map_err(|_| invalid())?,
            };
            properties.insert(name, value);
        }
    }
    let mut identity = NativeIdentity::new(&source)
        .qualified("native_id", properties.get("native_id").ok_or_else(invalid)?)
        .qualified("database_oid", database_oid);
    if let Some(start) = properties.get("native_start") {
        identity = identity.qualified("started_at", canonical_time(start)?);
    }
    let scope = match properties.get("native_scope").map(String::as_str) {
        Some("server") => MeasurementScope::Server,
        Some("database") => MeasurementScope::Database,
        _ => return Err(invalid()),
    };
    let mut counters = vec![];
    if let Some(value) = properties.get("xid_age") {
        let value = value.parse::<u64>().map_err(|_| invalid())?;
        counters.push(NativeCounterSample::gauge("xid_age", value, CounterUnit::Count, scope, &source, "xid_age"));
    }
    if let Some(value) = properties.get("seconds") {
        let value = value.parse::<f64>().ok().filter(|value| value.is_finite() && *value >= 0.0).ok_or_else(invalid)?;
        counters.push(NativeCounterSample {
            key: "age".into(),
            value: MeasurementValue::Real(value),
            unit: CounterUnit::Seconds,
            nature: CounterNature::Gauge,
            scope,
            reset: culprit_core::ResetMarker::Unknown,
            native_object: source.clone(),
            native_name: "seconds".into(),
            native_instance: None,
            native_type: None,
        });
    }
    let catalog = NativeCatalogDefinition {
        identity: identity.clone(),
        object_type: "cleanup-holder".into(),
        properties,
        definition: None,
        predicate: None,
    };
    let counters =
        NativeCounterSet { identity, scope, counters, properties: BTreeMap::from([("native_source".into(), source)]) };
    let record = NativeCatalogCounters { catalog, counters };
    if !record.is_valid() {
        return Err(invalid());
    }
    Ok(record)
}
pub(crate) fn project(kind: Kind, row: &Row) -> OpenTransaction {
    match kind {
        Kind::Session => OpenTransaction {
            session: row.string("session"),
            seconds: row.number("seconds").unwrap_or(0.0),
            program: row.text("program").filter(|value| !value.is_empty()),
            activity: if row.text("state").is_some_and(|state| state.starts_with("idle")) {
                TransactionActivity::IdleInTransaction
            } else {
                TransactionActivity::Reported
            },
            state: row.text("state"),
            holder: CleanupHolder::Session,
            xid_age: row.number("xid_age"),
            ..Default::default()
        },
        Kind::Slot => OpenTransaction {
            catalog_only: row.flag("catalog_only"),
            session: row.string("session"),
            activity: if row.flag("active") {
                TransactionActivity::SlotActive
            } else {
                TransactionActivity::SlotInactive
            },
            holder: CleanupHolder::ReplicationSlot,
            xid_age: row.number("xid_age"),
            ..Default::default()
        },
        Kind::Prepared => OpenTransaction {
            session: row.string("session"),
            seconds: row.number("seconds").unwrap_or(0.0),
            program: row.text("owner").filter(|value| !value.is_empty()),
            activity: TransactionActivity::Prepared,
            holder: CleanupHolder::PreparedTransaction,
            xid_age: row.number("xid_age"),
            ..Default::default()
        },
    }
}
