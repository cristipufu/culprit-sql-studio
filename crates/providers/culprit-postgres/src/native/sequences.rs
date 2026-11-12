//! Exact signed sequence capacity, read once by either consumer. A missing last value remains unknown.
use super::*;
use culprit_core::{MeasurementScope, operations::SequenceCapacity, telemetry::*};
use std::collections::BTreeMap;
pub(crate) fn query(limit: Option<usize>) -> String {
    let sql="SELECT sc.oid::text AS sequence_oid, (SELECT oid::text FROM pg_database WHERE datname=current_database()) AS database_oid, s.schemaname || '.' || s.sequencename AS name,
 own.table_name,
 s.last_value::text AS current_value,
 s.increment_by::text AS step,
 GREATEST(s.min_value, COALESCE(own.col_min, s.min_value))::text AS min_value,
 LEAST(s.max_value, COALESCE(own.col_max, s.max_value))::text AS max_value,
 s.cycle
 FROM pg_sequences s
 JOIN pg_namespace sn ON sn.nspname = s.schemaname
 JOIN pg_class sc ON sc.relnamespace = sn.oid AND sc.relname = s.sequencename AND sc.relkind = 'S'
 LEFT JOIN LATERAL (
     SELECT tn.nspname || '.' || tc.relname AS table_name,
            CASE a.atttypid WHEN 21 THEN -32768::bigint WHEN 23 THEN -2147483648::bigint END AS col_min,
            CASE a.atttypid WHEN 21 THEN 32767::bigint WHEN 23 THEN 2147483647::bigint END AS col_max
       FROM pg_depend d
       JOIN pg_class tc ON tc.oid = d.refobjid
       JOIN pg_namespace tn ON tn.oid = tc.relnamespace
       JOIN pg_attribute a ON a.attrelid = tc.oid AND a.attnum = d.refobjsubid AND NOT a.attisdropped
      WHERE d.classid = 'pg_class'::regclass
        AND d.objid = sc.oid
        AND d.refclassid = 'pg_class'::regclass
        AND d.refobjsubid > 0
        AND d.deptype IN ('a', 'i')
      LIMIT 1
 ) own ON true
 WHERE s.schemaname NOT IN ('pg_catalog', 'information_schema')";
    match limit {
        Some(limit) => format!("{sql} LIMIT {limit}"),
        None => sql.into(),
    }
}
pub(crate) fn decode(row: Row) -> Result<NativeCatalogCounters> {
    let identity = NativeIdentity::new(integer(&row, "sequence_oid")?.to_string())
        .qualified("database_oid", integer(&row, "database_oid")?.to_string());
    let mut properties = BTreeMap::new();
    for name in ["name", "table_name", "current_value", "step", "min_value", "max_value", "cycle"] {
        if let Some(value) = row.text(name) {
            properties.insert(name.into(), value);
        }
    }
    for name in ["current_value", "step", "min_value", "max_value"] {
        if let Some(value) = properties.get(name) {
            value.parse::<i64>().map_err(|_| invalid())?;
        }
    }
    let catalog = NativeCatalogDefinition {
        identity: identity.clone(),
        object_type: "sequence".into(),
        properties,
        definition: None,
        predicate: None,
    };
    let counters = NativeCounterSet {
        identity,
        scope: MeasurementScope::Database,
        counters: vec![],
        properties: BTreeMap::from([("native_source".into(), "pg_sequences".into())]),
    };
    let reading = NativeCatalogCounters { catalog, counters };
    if !reading.is_valid() {
        return Err(invalid());
    }
    Ok(reading)
}
pub(crate) fn project(reading: NativeCatalogCounters) -> Option<SequenceCapacity> {
    let values = &reading.catalog.properties;
    Some(SequenceCapacity {
        name: values.get("name")?.clone(),
        table: values.get("table_name").cloned(),
        current: values.get("current_value")?.clone(),
        increment: values.get("step")?.clone(),
        minimum: values.get("min_value")?.clone(),
        maximum: values.get("max_value")?.clone(),
        cycles: values.get("cycle").is_some_and(|value| value == "t" || value == "true"),
        allocated: true,
    })
}
