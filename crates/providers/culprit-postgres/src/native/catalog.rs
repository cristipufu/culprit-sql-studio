//! Native catalog metadata has one query/decoder per relation. User expressions are an explicit read policy.
use super::*;
use culprit_core::{ColumnInfo, ConstraintIssue, ConstraintKind, ForeignKey, telemetry::*};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub enum Kind {
    Columns,
    ForeignKeys,
    Checks,
    UnvalidatedChecks,
    UnvalidatedConstraints,
    Objects,
    Triggers,
    Partitions,
    Statistics,
}
impl Kind {
    pub fn dataset(self) -> SnapshotDataset {
        match self {
            Self::Columns => SnapshotDataset::Columns,
            Self::ForeignKeys => SnapshotDataset::ForeignKeys,
            Self::Checks | Self::UnvalidatedChecks | Self::UnvalidatedConstraints => SnapshotDataset::CheckConstraints,
            Self::Objects => SnapshotDataset::Objects,
            Self::Triggers => SnapshotDataset::Triggers,
            Self::Partitions => SnapshotDataset::Partitions,
            Self::Statistics => SnapshotDataset::Statistics,
        }
    }
    pub fn source(self) -> &'static str {
        match self {
            Self::Columns => "pg_attribute",
            Self::ForeignKeys | Self::Checks | Self::UnvalidatedChecks | Self::UnvalidatedConstraints => {
                "pg_constraint"
            }
            Self::Objects | Self::Partitions => "pg_class",
            Self::Triggers => "pg_trigger",
            Self::Statistics => "pg_stats",
        }
    }
    pub fn object_type(self) -> &'static str {
        match self {
            Self::Columns => "column",
            Self::ForeignKeys => "foreign-key",
            Self::Checks | Self::UnvalidatedChecks => "check-constraint",
            Self::UnvalidatedConstraints => "constraint",
            Self::Objects => "relation",
            Self::Triggers => "trigger",
            Self::Partitions => "partition",
            Self::Statistics => "column-statistics",
        }
    }
}
pub struct CatalogReading {
    pub definition: NativeCatalogDefinition,
    row: Row,
}
pub const USER_SCHEMA: &str = "n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp%'";
pub fn query(kind: Kind, version: u32, deparse: bool, limit: usize) -> String {
    let generated = if version >= 120_000 { "a.attgenerated::text" } else { "NULL::text" };
    let root_constraint = if version >= 110_000 { "c.conparentid=0" } else { "true" };
    let definition = |expression: &str| if deparse { expression.to_owned() } else { "NULL::text".into() };
    let check_filter = if matches!(kind, Kind::UnvalidatedChecks | Kind::UnvalidatedConstraints) {
        "AND NOT k.convalidated"
    } else {
        ""
    };
    let check_type =
        if matches!(kind, Kind::UnvalidatedConstraints) { "k.contype IN ('c','f')" } else { "k.contype='c'" };
    let relation=match kind {
        Kind::Columns=>format!("SELECT a.attrelid::text||':'||a.attnum::text AS native_id,
            jsonb_build_object('schema_name',n.nspname,'table_name',c.relname,'column_name',a.attname,
            'relation_oid',a.attrelid::text,'ordinal',a.attnum::text,'type_oid',a.atttypid::text,'type_modifier',a.atttypmod::text,
            'data_type',pg_catalog.format_type(a.atttypid,a.atttypmod),'nullable',NOT a.attnotnull,
            'identity',a.attidentity::text,'generated',{generated},'collation_oid',a.attcollation::text,
            'statistics_target',a.attstattarget::text,'definition_md5',md5(d.adbin::text)) AS native_values,
            {} AS definition, (d.adbin IS NOT NULL) AS has_definition
            FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid=a.attrelid
            JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
            LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum
            WHERE c.relkind IN ('r','p','m') AND a.attnum>0 AND NOT a.attisdropped AND {USER_SCHEMA}",definition("pg_catalog.pg_get_expr(d.adbin,d.adrelid)")),
        Kind::ForeignKeys=>format!("SELECT c.oid::text AS native_id,
            jsonb_build_object('name',c.conname,'schema_name',n.nspname,'table_name',t.relname,
            'relation_oid',c.conrelid::text,'ref_schema',rn.nspname,'ref_table',rt.relname,'ref_relation_oid',c.confrelid::text,
            'column_numbers',c.conkey::text,'ref_column_numbers',c.confkey::text,
            'delete_rule',c.confdeltype::text,'update_rule',c.confupdtype::text,'match_type',c.confmatchtype::text,
            'validated',c.convalidated,'deferrable',c.condeferrable,'deferred',c.condeferred,
            'columns',(SELECT json_agg(a.attname ORDER BY u.ord) FROM unnest(c.conkey) WITH ORDINALITY u(attnum,ord) JOIN pg_catalog.pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=u.attnum),
            'ref_columns',(SELECT json_agg(a.attname ORDER BY u.ord) FROM unnest(c.confkey) WITH ORDINALITY u(attnum,ord) JOIN pg_catalog.pg_attribute a ON a.attrelid=c.confrelid AND a.attnum=u.attnum)) AS native_values,
            NULL::text AS definition,false AS has_definition
            FROM pg_catalog.pg_constraint c JOIN pg_catalog.pg_class t ON t.oid=c.conrelid
            JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace JOIN pg_catalog.pg_class rt ON rt.oid=c.confrelid
            JOIN pg_catalog.pg_namespace rn ON rn.oid=rt.relnamespace
            WHERE c.contype='f' AND {root_constraint} AND {USER_SCHEMA}"),
        Kind::Checks|Kind::UnvalidatedChecks|Kind::UnvalidatedConstraints=>format!("SELECT k.oid::text AS native_id,
            jsonb_build_object('name',k.conname,'schema_name',n.nspname,'table_name',c.relname,'relation_oid',k.conrelid::text,
            'constraint_type',k.contype::text,'validated',k.convalidated,'no_inherit',k.connoinherit,'inheritance_count',k.coninhcount::text,
            'inheritance_child',EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhrelid=c.oid) AND NOT c.relispartition,
            'column_numbers',k.conkey::text,'definition_md5',md5(k.conbin::text)) AS native_values,
            {} AS definition,(k.conbin IS NOT NULL) AS has_definition
            FROM pg_catalog.pg_constraint k JOIN pg_catalog.pg_class c ON c.oid=k.conrelid
            JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE {check_type} AND {USER_SCHEMA} {check_filter}",definition("pg_catalog.pg_get_expr(k.conbin,k.conrelid)")),
        Kind::Objects|Kind::Partitions=>format!("SELECT c.oid::text AS native_id,
            jsonb_build_object('name',c.relname,'schema_name',n.nspname,'kind',c.relkind::text,
            'owner_oid',c.relowner::text,'persistence',c.relpersistence::text,'access_method_oid',c.relam::text,
            'replica_identity',c.relreplident::text,'options',c.reloptions::text,'partition_key',p.partattrs::text,
            'partition_strategy',p.partstrat::text,'partition_bound_md5',md5(c.relpartbound::text),
            'parents',(SELECT array_agg(i.inhparent ORDER BY i.inhseqno)::text FROM pg_catalog.pg_inherits i WHERE i.inhrelid=c.oid)) AS native_values,
            NULL::text AS definition,{} AS has_definition
            FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
            LEFT JOIN pg_catalog.pg_partitioned_table p ON p.partrelid=c.oid
            WHERE c.relkind IN ('r','p','v','m','i','I','S','f') AND {USER_SCHEMA} {}",
            if matches!(kind,Kind::Partitions) {"c.relpartbound IS NOT NULL"} else {"false"},
            if matches!(kind,Kind::Partitions) {"AND (c.relispartition OR p.partrelid IS NOT NULL)"} else {""}),
        Kind::Triggers=>format!("SELECT t.oid::text AS native_id,
            jsonb_build_object('name',t.tgname,'schema_name',n.nspname,'table_name',c.relname,'relation_oid',t.tgrelid::text,
            'function_oid',t.tgfoid::text,'enabled',t.tgenabled::text,'internal',t.tgisinternal,'type',t.tgtype::text,
            'deferrable',t.tgdeferrable,'deferred',t.tginitdeferred,'columns',t.tgattr::text,
            'condition_md5',md5(t.tgqual::text)) AS native_values,NULL::text AS definition,false AS has_definition
            FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid
            JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE NOT t.tgisinternal AND {USER_SCHEMA}"),
        Kind::Statistics=>"SELECT c.oid::text||':'||a.attnum::text||':'||s.inherited::text AS native_id,
            jsonb_build_object('schema_name',s.schemaname,'table_name',s.tablename,'column_name',s.attname,
            'inherited',s.inherited,'null_frac',s.null_frac::text,'average_width',s.avg_width::text,
            'distinct_values',s.n_distinct::text,'correlation',s.correlation::text) AS native_values,
            NULL::text AS definition,false AS has_definition FROM pg_catalog.pg_stats s
            JOIN pg_catalog.pg_namespace n ON n.nspname=s.schemaname
            JOIN pg_catalog.pg_class c ON c.relnamespace=n.oid AND c.relname=s.tablename
            JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid AND a.attname=s.attname
            WHERE s.schemaname NOT IN ('pg_catalog','information_schema')".into(),
    };
    format!(
        "SELECT native_id,native_values::text,definition,has_definition FROM ({relation}) native_catalog LIMIT {limit}"
    )
}
pub fn decode(
    kind: Kind,
    row: Row,
    database_oid: Option<&str>,
    max_sql_bytes: usize,
    engine: culprit_core::Engine,
) -> Result<CatalogReading> {
    let mut identity = NativeIdentity::new(text(&row, "native_id")?);
    if let Some(database_oid) = database_oid {
        identity = identity.qualified("database_oid", database_oid);
    }
    let values: BTreeMap<String, Value> = serde_json::from_str(&text(&row, "native_values")?).map_err(|_| invalid())?;
    let mut properties = BTreeMap::new();
    for (name, value) in values {
        if !value.is_null() {
            let value = match value {
                Value::String(value) => value,
                other => serde_json::to_string(&other).map_err(|_| invalid())?,
            };
            properties.insert(name, value);
        }
    }
    let definition = match row.text("definition") {
        Some(sql) => Some(CatalogExpression::sql(&sql, engine, max_sql_bytes)),
        None if flag(&row, "has_definition")? => Some(CatalogExpression {
            native_sha256: None,
            payload: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        }),
        None => None,
    };
    let definition = NativeCatalogDefinition {
        identity,
        object_type: kind.object_type().into(),
        properties,
        definition,
        predicate: None,
    };
    if !definition.is_valid() {
        return Err(invalid());
    }
    Ok(CatalogReading { definition, row })
}
fn label(reading: &CatalogReading, name: &str) -> String {
    reading.definition.properties.get(name).cloned().unwrap_or_default()
}
fn list(reading: &CatalogReading, name: &str) -> Result<Vec<String>> {
    serde_json::from_str(&label(reading, name)).map_err(|_| invalid())
}
pub fn column(reading: CatalogReading) -> Result<ColumnInfo> {
    let default = reading.row.text("definition");
    let ordinal = label(&reading, "ordinal").parse::<u32>().map_err(|_| invalid())?;
    Ok(ColumnInfo {
        schema: label(&reading, "schema_name"),
        table: label(&reading, "table_name"),
        name: label(&reading, "column_name"),
        data_type: label(&reading, "data_type"),
        nullable: label(&reading, "nullable") == "true",
        generated: !label(&reading, "identity").is_empty()
            || !label(&reading, "generated").is_empty()
            || default.as_deref().is_some_and(|value| value.starts_with("nextval(")),
        default,
        ordinal,
    })
}
pub fn foreign_key(reading: CatalogReading) -> Result<ForeignKey> {
    Ok(ForeignKey {
        name: label(&reading, "name"),
        schema: label(&reading, "schema_name"),
        table: label(&reading, "table_name"),
        columns: list(&reading, "columns")?,
        ref_schema: label(&reading, "ref_schema"),
        ref_table: label(&reading, "ref_table"),
        ref_columns: list(&reading, "ref_columns")?,
        delete_rule: match label(&reading, "delete_rule").as_str() {
            "c" => Some("CASCADE".into()),
            "n" => Some("SET NULL".into()),
            "d" => Some("SET DEFAULT".into()),
            "r" => Some("RESTRICT".into()),
            _ => None,
        },
    })
}
pub fn constraint(reading: CatalogReading) -> Option<ConstraintIssue> {
    if label(&reading, "validated") != "false" {
        return None;
    }
    Some(ConstraintIssue {
        schema: label(&reading, "schema_name"),
        table: label(&reading, "table_name"),
        name: label(&reading, "name"),
        kind: if label(&reading, "constraint_type") == "f" {
            ConstraintKind::ForeignKey
        } else {
            ConstraintKind::Check
        },
        disabled: false,
        inheritance_child: label(&reading, "inheritance_child") == "true",
    })
}

pub async fn statistics_visible<R: QueryRead>(reader: &mut R) -> Result<bool> {
    let row=one(reader,&format!("SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
        WHERE c.relkind IN ('r','p','m') AND {USER_SCHEMA}
        AND (NOT pg_catalog.has_table_privilege(c.oid,'SELECT') OR pg_catalog.row_security_active(c.oid))) AS visible")).await?;
    flag(&row, "visible")
}
