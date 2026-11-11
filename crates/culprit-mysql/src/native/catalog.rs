//! Allowlisted catalog projections. Defaults, histogram values and stored program bodies are never metadata.
use super::*;
use crate::connect::Flavor;
use culprit_core::{MeasurementScope, telemetry::*};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Kind {
    Tables,
    Indexes,
    Columns,
    ForeignKeys,
    Checks,
    Objects,
    Triggers,
    Partitions,
    Statistics,
    Histograms,
    Sequences,
}
impl Kind {
    pub(crate) fn dataset(self) -> SnapshotDataset {
        match self {
            Self::Tables => SnapshotDataset::Tables,
            Self::Indexes => SnapshotDataset::Indexes,
            Self::Columns => SnapshotDataset::Columns,
            Self::ForeignKeys => SnapshotDataset::ForeignKeys,
            Self::Checks => SnapshotDataset::CheckConstraints,
            Self::Objects => SnapshotDataset::Objects,
            Self::Triggers => SnapshotDataset::Triggers,
            Self::Partitions => SnapshotDataset::Partitions,
            Self::Statistics | Self::Histograms => SnapshotDataset::Statistics,
            Self::Sequences => SnapshotDataset::Sequences,
        }
    }
    pub(crate) fn source(self) -> &'static str {
        match self {
            Self::Tables | Self::Sequences => "information_schema.tables",
            Self::Indexes | Self::Statistics => "information_schema.statistics",
            Self::Columns => "information_schema.columns",
            Self::Histograms => "column-histogram-metadata",
            Self::ForeignKeys => "information_schema.key_column_usage",
            Self::Checks => "information_schema.table_constraints",
            Self::Objects => "information_schema.routines",
            Self::Triggers => "information_schema.triggers",
            Self::Partitions => "information_schema.partitions",
        }
    }
    fn object_type(self) -> &'static str {
        match self {
            Self::Tables => "table",
            Self::Indexes => "index-column",
            Self::Columns => "column",
            Self::ForeignKeys => "foreign-key-column",
            Self::Checks => "check-constraint",
            Self::Objects => "routine",
            Self::Triggers => "trigger",
            Self::Partitions => "partition",
            Self::Statistics => "index-column-statistics",
            Self::Histograms => "column-histogram",
            Self::Sequences => "auto-increment",
        }
    }
    pub(crate) fn query(self, flavor: Flavor, limit: usize) -> String {
        let sql = match self {
            Self::Tables => tables_query(),
            Self::Histograms => histogram_query(flavor),
            Self::Columns => columns_query(false),
            Self::Indexes | Self::Statistics => indexes_query(flavor == Flavor::Mysql, false),
            Self::ForeignKeys => foreign_keys_query(),
            Self::Checks => "SELECT TABLE_NAME AS table_name, CONSTRAINT_NAME AS name
                FROM information_schema.TABLE_CONSTRAINTS WHERE CONSTRAINT_SCHEMA=? AND CONSTRAINT_TYPE='CHECK'"
                .into(),
            Self::Objects => "SELECT ROUTINE_NAME AS name, ROUTINE_TYPE AS routine_type, DATA_TYPE AS data_type,
                SECURITY_TYPE AS security_type, IS_DETERMINISTIC AS is_deterministic, SQL_DATA_ACCESS AS sql_data_access
                FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA=?"
                .into(),
            Self::Triggers => {
                "SELECT TRIGGER_NAME AS name, EVENT_OBJECT_TABLE AS table_name, EVENT_MANIPULATION AS event,
                ACTION_TIMING AS timing, CAST(ACTION_ORDER AS CHAR) AS action_order
                FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA=?"
                    .into()
            }
            Self::Partitions => {
                "SELECT TABLE_NAME AS table_name, PARTITION_NAME AS name, SUBPARTITION_NAME AS subpartition_name,
                PARTITION_METHOD AS method, SUBPARTITION_METHOD AS subpartition_method,
                CAST(PARTITION_ORDINAL_POSITION AS CHAR) AS ordinal, TABLE_ROWS AS row_estimate,
                DATA_LENGTH AS data_bytes, INDEX_LENGTH AS index_bytes
                FROM information_schema.PARTITIONS WHERE TABLE_SCHEMA=? AND PARTITION_NAME IS NOT NULL"
                    .into()
            }
            Self::Sequences => AUTO_INCREMENT_QUERY.into(),
        };
        format!("{sql} LIMIT {limit}")
    }
    pub(crate) fn decode(self, row: &Rec, database: &str) -> Result<TelemetryRecord> {
        let keys: &[&str] = match self {
            Self::Tables | Self::Objects => &["name"],
            Self::Indexes | Self::Statistics => &["table_name", "index_name", "seq"],
            Self::Columns => &["table_name", "name"],
            Self::Histograms => &["table_name", "column_name"],
            Self::ForeignKeys => &["table_name", "name", "column_name"],
            Self::Checks | Self::Triggers => &["table_name", "name"],
            Self::Partitions => &["table_name", "name", "subpartition_name"],
            Self::Sequences => &["name"],
        };
        let mut properties = BTreeMap::new();
        for &name in self.properties() {
            if let Some(value) = row.native_text(name)? {
                properties.insert(name.into(), value);
            }
        }
        let identity_parts = keys.iter().map(|key| row.native_text(key)).collect::<Result<Vec<_>>>()?;
        if identity_parts.iter().all(Option::is_none) {
            return Err(invalid());
        }
        let identity = NativeIdentity::new(serde_json::to_string(&identity_parts).map_err(|_| invalid())?)
            .qualified("schema", database);
        let definition = NativeCatalogDefinition {
            identity: identity.clone(),
            object_type: self.object_type().into(),
            properties,
            definition: None,
            predicate: None,
        };
        if matches!(self, Self::Tables | Self::Indexes | Self::Statistics | Self::Histograms | Self::Sequences) {
            let counters = self.counters(row)?;
            let record = NativeCatalogCounters {
                catalog: definition,
                counters: NativeCounterSet {
                    identity,
                    scope: MeasurementScope::Database,
                    counters,
                    properties: BTreeMap::from([("object_type".into(), self.object_type().into())]),
                },
            };
            if !record.is_valid() {
                return Err(invalid());
            }
            Ok(TelemetryRecord::Operational(OperationalObservation {
                source: self.source().into(),
                key: record.catalog.key()?,
                fact: OperationalFact::CatalogCounters(Box::new(record)),
            }))
        } else {
            Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
                self.source().into(),
                ObjectDefinitionFact::NativeCatalog(Box::new(definition)),
            )?))
        }
    }
    fn properties(self) -> &'static [&'static str] {
        match self {
            Self::Tables => &["name", "engine", "table_type", "has_pk"],
            Self::Columns => &["table_name", "name", "data_type", "nullable", "extra", "ordinal", "collation_name"],
            Self::Indexes | Self::Statistics => &[
                "table_name",
                "index_name",
                "non_unique",
                "seq",
                "column_name",
                "collation",
                "sub_part",
                "index_type",
                "visible",
            ],
            Self::ForeignKeys => &[
                "table_name",
                "name",
                "column_name",
                "ref_schema",
                "ref_table",
                "ref_column",
                "delete_rule",
                "update_rule",
                "ordinal",
            ],
            Self::Checks => &["table_name", "name"],
            Self::Objects => {
                &["name", "routine_type", "data_type", "security_type", "is_deterministic", "sql_data_access"]
            }
            Self::Triggers => &["table_name", "name", "event", "timing", "action_order"],
            Self::Partitions => {
                &["table_name", "name", "subpartition_name", "method", "subpartition_method", "ordinal"]
            }
            Self::Sequences => &["name", "table_name", "column_type", "step"],
            Self::Histograms => &["schema_name", "table_name", "column_name", "kind"],
        }
    }
    fn counters(self, row: &Rec) -> Result<Vec<NativeCounterSample>> {
        let specs: &[(&str, CounterUnit)] = match self {
            Self::Tables => &[
                ("row_estimate", CounterUnit::Count),
                ("data_bytes", CounterUnit::Bytes),
                ("index_bytes", CounterUnit::Bytes),
            ],
            Self::Indexes | Self::Statistics => &[("cardinality", CounterUnit::Count)],
            Self::Sequences => &[("next_value", CounterUnit::Count)],
            Self::Histograms => &[("buckets", CounterUnit::Count)],
            _ => &[],
        };
        let mut counters = vec![];
        for &(name, unit) in specs {
            if let Some(value) = row.unsigned(name)? {
                counters.push(NativeCounterSample::gauge(
                    name,
                    value,
                    unit,
                    MeasurementScope::Database,
                    self.source(),
                    name,
                ));
            }
        }
        Ok(counters)
    }
}

pub(crate) fn tables_query() -> String {
    "SELECT t.TABLE_NAME AS name, t.TABLE_ROWS AS row_estimate, t.DATA_LENGTH AS data_bytes, t.INDEX_LENGTH AS index_bytes,
        t.ENGINE AS engine, t.TABLE_TYPE AS table_type,
        CAST(EXISTS (SELECT 1 FROM information_schema.TABLE_CONSTRAINTS c
            WHERE c.TABLE_SCHEMA=t.TABLE_SCHEMA AND c.TABLE_NAME=t.TABLE_NAME AND c.CONSTRAINT_TYPE='PRIMARY KEY') AS CHAR) AS has_pk
        FROM information_schema.TABLES t WHERE t.TABLE_SCHEMA=? AND t.TABLE_TYPE='BASE TABLE'".into()
}
pub(crate) fn columns_query(defaults: bool) -> String {
    let default = if defaults { "COLUMN_DEFAULT" } else { "NULL" };
    format!(
        "SELECT TABLE_NAME AS table_name, COLUMN_NAME AS name, COLUMN_TYPE AS data_type,
        IS_NULLABLE AS nullable, {default} AS default_value, EXTRA AS extra,
        CAST(ORDINAL_POSITION AS CHAR) AS ordinal, COLLATION_NAME AS collation_name
        FROM information_schema.COLUMNS WHERE TABLE_SCHEMA=? ORDER BY TABLE_NAME, ORDINAL_POSITION"
    )
}
pub(crate) fn indexes_query(visibility: bool, expressions: bool) -> String {
    let visible = if visibility { "IS_VISIBLE" } else { "NULL" };
    let expression = if visibility && expressions { "EXPRESSION" } else { "NULL" };
    // Expression bodies are fetched separately by interactive scans, never in the metadata export.
    format!(
        "SELECT TABLE_NAME AS table_name, INDEX_NAME AS index_name, CAST(NON_UNIQUE AS CHAR) AS non_unique,
        CAST(SEQ_IN_INDEX AS CHAR) AS seq, COLUMN_NAME AS column_name, COLLATION AS collation,
        CAST(SUB_PART AS CHAR) AS sub_part, INDEX_TYPE AS index_type, {visible} AS visible,
        CARDINALITY AS cardinality, {expression} AS expression FROM information_schema.STATISTICS WHERE TABLE_SCHEMA=?
        ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX"
    )
}
pub(crate) fn foreign_keys_query() -> String {
    "SELECT k.CONSTRAINT_NAME AS name, k.TABLE_NAME AS table_name, k.COLUMN_NAME AS column_name,
        k.REFERENCED_TABLE_SCHEMA AS ref_schema, k.REFERENCED_TABLE_NAME AS ref_table,
        k.REFERENCED_COLUMN_NAME AS ref_column, rc.DELETE_RULE AS delete_rule, rc.UPDATE_RULE AS update_rule,
        CAST(k.ORDINAL_POSITION AS CHAR) AS ordinal
        FROM information_schema.KEY_COLUMN_USAGE k LEFT JOIN information_schema.REFERENTIAL_CONSTRAINTS rc
        ON rc.CONSTRAINT_SCHEMA=k.TABLE_SCHEMA AND rc.TABLE_NAME=k.TABLE_NAME AND rc.CONSTRAINT_NAME=k.CONSTRAINT_NAME
        WHERE k.TABLE_SCHEMA=? AND k.REFERENCED_TABLE_NAME IS NOT NULL ORDER BY k.TABLE_NAME,k.CONSTRAINT_NAME,k.ORDINAL_POSITION".into()
}
pub(crate) const AUTO_INCREMENT_QUERY: &str =
    "SELECT CONCAT(t.TABLE_SCHEMA,'.',t.TABLE_NAME,'.',c.COLUMN_NAME) AS name,
    CONCAT(t.TABLE_SCHEMA,'.',t.TABLE_NAME) AS table_name, CAST(t.AUTO_INCREMENT AS CHAR) AS next_value,
    c.COLUMN_TYPE AS column_type, CAST(@@GLOBAL.auto_increment_increment AS CHAR) AS step
    FROM information_schema.TABLES t JOIN information_schema.COLUMNS c
    ON c.TABLE_SCHEMA=t.TABLE_SCHEMA AND c.TABLE_NAME=t.TABLE_NAME
    WHERE t.TABLE_SCHEMA=? AND c.EXTRA LIKE '%auto_increment%' AND t.AUTO_INCREMENT IS NOT NULL";

pub(crate) fn histogram_query(flavor: Flavor) -> String {
    match flavor {
        Flavor::Mariadb=>"SELECT db_name AS schema_name,table_name,column_name,hist_type AS kind,NULL AS buckets FROM mysql.column_stats WHERE db_name=?".into(),
        Flavor::Mysql=>"SELECT SCHEMA_NAME AS schema_name,TABLE_NAME AS table_name,COLUMN_NAME AS column_name,JSON_UNQUOTE(JSON_EXTRACT(HISTOGRAM,'$.\"histogram-type\"')) AS kind,JSON_LENGTH(JSON_EXTRACT(HISTOGRAM,'$.buckets')) AS buckets FROM information_schema.COLUMN_STATISTICS WHERE SCHEMA_NAME=?".into(),
    }
}
