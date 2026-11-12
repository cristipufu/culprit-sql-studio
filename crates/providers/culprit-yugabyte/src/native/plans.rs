//! Native QPM cache, guarded before touching the view's known unsafe foreign-role redaction path.
use super::{metadata::Metadata, *};
use culprit_core::{Engine, MeasurementScope};
use std::collections::BTreeMap;
pub(crate) const SOURCE: &str = "yb_pg_stat_plans";
pub(crate) fn query(meta: &Metadata, limit: usize, maximum_bytes: usize) -> Result<String> {
    crate::history::authorize(&meta.probe)?;
    Ok(format!(
        "SELECT userid::text AS userid,dbid::text AS dbid,queryid::text AS queryid,planid::text AS planid,calls::text AS calls,avg_exec_time::text AS mean_ms,max_exec_time::text AS max_ms,{} AS first_used,{} AS last_used,pg_catalog.octet_length(plan)::text AS plan_bytes,CASE WHEN pg_catalog.octet_length(plan)<={maximum_bytes} THEN plan ELSE NULL END AS plan FROM {} WHERE dbid={} AND {} LIMIT {limit}",
        crate::probe::timestamp("first_used"),
        crate::probe::timestamp("last_used"),
        crate::history::guarded_relation("pg_catalog.yb_pg_stat_plans"),
        meta.probe.oid,
        crate::probe::ALL_STATS
    ))
}
pub(crate) fn decode(
    row: &Row,
    meta: &Metadata,
    source: &CollectionDescription,
    maximum_bytes: usize,
) -> Result<TelemetryRecord> {
    let query_id = text(row, "queryid")?;
    query_id.parse::<i64>().map_err(|_| invalid())?;
    let plan_id = text(row, "planid")?;
    plan_id.parse::<i64>().map_err(|_| invalid())?;
    if integer(row, "dbid")? != u64::from(meta.probe.oid) {
        return Err(invalid());
    }
    let query = NativeQueryIdentity {
        query_id,
        qualifiers: BTreeMap::from([
            ("node".into(), meta.node.clone()),
            ("database_oid".into(), meta.probe.oid.to_string()),
            ("userid".into(), integer(row, "userid")?.to_string()),
        ]),
    };
    let identity = NativePlanIdentity {
        plan_id,
        query: query.clone(),
        qualifiers: BTreeMap::from([("node".into(), meta.node.clone()), ("cache".into(), SOURCE.into())]),
    };
    let id = identity.definition_id(source)?;
    let bytes = row.text("plan_bytes").map(|value| value.parse::<u64>().map_err(|_| invalid())).transpose()?;
    let plan = match row.text("plan") {
        Some(plan) if plan.len() <= maximum_bytes => DefinitionPayload::plan(&plan, Engine::Yugabyte),
        Some(_) => return Err(invalid()),
        None => DefinitionPayload::Unavailable {
            reason: if bytes.is_some_and(|bytes| bytes > maximum_bytes as u64) {
                PayloadUnavailable::Oversized
            } else {
                PayloadUnavailable::Missing
            },
        },
    };
    let mut counters = vec![NativeCounterSample {
        nature: CounterNature::Cumulative,
        ..NativeCounterSample::gauge(
            "executions",
            integer(row, "calls")?,
            CounterUnit::Count,
            MeasurementScope::Node,
            SOURCE,
            "calls",
        )
    }];
    for name in ["mean_ms", "max_ms"] {
        let mut counter =
            NativeCounterSample::gauge(name, 0, CounterUnit::Milliseconds, MeasurementScope::Node, SOURCE, name);
        counter.value = MeasurementValue::Real(number(row, name)?);
        counters.push(counter);
    }
    let definition = NativeRetainedPlan {
        definition: PlanDefinition {
            id: id.clone(),
            identity,
            query_definition_id: None,
            engine_version: Some(meta.probe.version.clone()),
            native_properties: BTreeMap::from([("cache".into(), SOURCE.into())]),
            source_text_bytes: bytes.map(|bytes| bytes.to_string()),
            plan,
        },
        counters: NativeCounterSet {
            identity: NativeIdentity::new(&id).qualified("node", &meta.node),
            scope: MeasurementScope::Node,
            counters,
            properties: BTreeMap::from([("cache-lifetime".into(), "independent".into())]),
        },
        first_used: row.text("first_used"),
        last_used: row.text("last_used"),
    };
    Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
        SOURCE.into(),
        ObjectDefinitionFact::RetainedPlan(Box::new(definition)),
    )?))
}
