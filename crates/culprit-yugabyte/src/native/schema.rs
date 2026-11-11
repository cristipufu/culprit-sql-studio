//! Catalog structure and estimates only. DocDB is not a PostgreSQL heap: no relpages, vacuum or physical-size calls.
use super::metadata::Metadata;
use super::*;
use culprit_core::{Engine, MeasurementScope};
use culprit_postgres::wire::catalog;
use std::collections::BTreeMap;

pub(crate) const SETTINGS: &[&str] = &[
    "yb_enable_cbo",
    "yb_enable_optimizer_statistics",
    "yb_bnl_batch_size",
    "yb_fetch_row_limit",
    "yb_fetch_size_limit",
    "yb_enable_expression_pushdown",
    "yb_enable_scalar_array_pushdown",
    "yb_enable_index_aggregate_pushdown",
    "yb_enable_base_scans_cost_model",
    "yb_enable_global_views",
    "yb_enable_pg_locks",
    "yb_locks_min_txn_age",
    "yb_enable_pg_stat_statements_docdb_metrics",
    "yb_enable_pg_stat_statements_rpc_stats",
    "work_mem",
    "statement_timeout",
    "lock_timeout",
    "default_statistics_target",
    "idle_in_transaction_session_timeout",
    "default_transaction_read_only",
    "max_connections",
    "plan_cache_mode",
    "pg_stat_statements.max",
    "pg_stat_statements.track",
    "pg_stat_statements.track_planning",
    "track_activities",
];
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Tables,
    Indexes,
    Catalog(catalog::Kind),
    Settings,
}
impl Kind {
    pub(crate) fn dataset(self) -> SnapshotDataset {
        match self {
            Self::Tables => SnapshotDataset::Tables,
            Self::Indexes => SnapshotDataset::Indexes,
            Self::Catalog(kind) => kind.dataset(),
            Self::Settings => SnapshotDataset::Settings,
        }
    }
    pub(crate) fn source(self) -> &'static str {
        match self {
            Self::Tables => "pg_class",
            Self::Indexes => "pg_index",
            Self::Catalog(kind) => kind.source(),
            Self::Settings => "pg_settings",
        }
    }
    pub(crate) fn query(self, meta: &Metadata, limit: usize) -> String {
        match self {
            Self::Settings=>culprit_postgres::wire::settings::query(SETTINGS,limit),
            Self::Catalog(kind)=>catalog::query(kind,meta.probe.compatibility,false,limit),
            Self::Tables=>format!("SELECT c.oid::text AS native_id,
                jsonb_build_object('schema_name',n.nspname,'name',c.relname,'kind',c.relkind::text,
                    'persistence',c.relpersistence::text,'owner_oid',c.relowner::text) AS native_values,
                c.reltuples::text AS row_estimate FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
                WHERE c.relkind IN ('r','p','m') AND {USER_SCHEMA} LIMIT {limit}"),
            Self::Indexes=>format!("SELECT i.oid::text AS native_id,
                jsonb_build_object('schema_name',n.nspname,'name',i.relname,'table_name',t.relname,'relation_oid',x.indrelid::text,
                    'access_method',am.amname,'primary',x.indisprimary,'unique',x.indisunique,'ready',x.indisready,'valid',x.indisvalid,
                    'key_numbers',x.indkey::text,'key_count',x.indnkeyatts::text,'attribute_count',x.indnatts::text,
                    'expression_md5',md5(x.indexprs::text),'predicate_md5',md5(x.indpred::text)) AS native_values
                FROM pg_catalog.pg_index x JOIN pg_catalog.pg_class i ON i.oid=x.indexrelid
                JOIN pg_catalog.pg_class t ON t.oid=x.indrelid JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace
                JOIN pg_catalog.pg_am am ON am.oid=i.relam WHERE {USER_SCHEMA} LIMIT {limit}"),
        }
    }
    pub(crate) fn decode(self, row: &Row, meta: &Metadata, sql_bytes: usize) -> Result<TelemetryRecord> {
        if matches!(self, Self::Settings) {
            let mut setting =
                culprit_postgres::wire::settings::decode_native(row, SETTINGS.iter().copied(), MeasurementScope::Node)?;
            setting.namespace = format!("pg_catalog.pg_settings@{}", meta.node);
            return Ok(TelemetryRecord::Setting(setting));
        }
        let mut definition = match self {
            Self::Catalog(kind) => {
                catalog::decode(kind, row.clone(), Some(&meta.probe.oid.to_string()), sql_bytes, Engine::Yugabyte)?
                    .definition
            }
            Self::Tables | Self::Indexes => NativeCatalogDefinition {
                identity: NativeIdentity::new(text(row, "native_id")?)
                    .qualified("database_oid", meta.probe.oid.to_string()),
                object_type: if matches!(self, Self::Tables) { "docdb-table" } else { "docdb-index" }.into(),
                properties: serde_json::from_str::<BTreeMap<String, serde_json::Value>>(&text(row, "native_values")?)
                    .map_err(|_| invalid())?
                    .into_iter()
                    .filter(|(_, value)| !value.is_null())
                    .map(|(key, value)| {
                        let value = match value {
                            serde_json::Value::String(value) => value,
                            other => other.to_string(),
                        };
                        (key, value)
                    })
                    .collect(),
                definition: None,
                predicate: None,
            },
            _ => unreachable!(),
        };
        definition.identity.qualifiers.insert("node".into(), meta.node.clone());
        if matches!(self, Self::Tables | Self::Indexes | Self::Catalog(catalog::Kind::Statistics)) {
            let mut counters = vec![];
            if matches!(self, Self::Tables)
                && let Some(estimate) = row.text("row_estimate")
            {
                let estimate = estimate.parse::<f64>().map_err(|_| invalid())?;
                if !estimate.is_finite() {
                    return Err(invalid());
                }
                if estimate >= 0.0 {
                    counters.push(NativeCounterSample {
                        value: MeasurementValue::Real(estimate),
                        ..NativeCounterSample::gauge(
                            "estimated-rows",
                            0,
                            CounterUnit::Count,
                            MeasurementScope::Node,
                            "pg_class",
                            "reltuples",
                        )
                    });
                }
            }
            let measurements = NativeCounterSet {
                identity: definition.identity.clone(),
                scope: MeasurementScope::Node,
                counters,
                properties: BTreeMap::from([("object_type".into(), definition.object_type.clone())]),
            };
            let key = definition.key()?;
            Ok(TelemetryRecord::Operational(OperationalObservation {
                source: self.source().into(),
                key,
                fact: OperationalFact::CatalogCounters(Box::new(NativeCatalogCounters {
                    catalog: definition,
                    counters: measurements,
                })),
            }))
        } else {
            Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
                self.source().into(),
                ObjectDefinitionFact::NativeCatalog(Box::new(definition)),
            )?))
        }
    }
}
use culprit_postgres::wire::catalog::USER_SCHEMA;
