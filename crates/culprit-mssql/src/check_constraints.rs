//! The full definition and app issue projection share the native constraint relation and flags.
use crate::{
    read::{boolean, integer, invalid_data, text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
const PARENT_RELATION: &str = "FROM sys.check_constraints cc JOIN sys.tables t ON t.object_id=cc.parent_object_id JOIN sys.schemas s ON s.schema_id=t.schema_id";
pub(crate) fn issues_sql() -> String {
    format!(
        "SELECT s.name AS schema_name,t.name AS table_name,cc.name,1 AS kind,CAST(cc.is_disabled AS int) AS disabled {PARENT_RELATION} WHERE (cc.is_not_trusted=1 OR cc.is_disabled=1) AND cc.is_not_for_replication=0 AND cc.is_ms_shipped=0 AND t.is_ms_shipped=0"
    )
}
pub(crate) fn select_sql(limit: usize) -> String {
    let definition = crate::catalog_expression::select("cc.definition", "definition", limit);
    format!(
        r#"
SELECT TOP (@P1) DB_ID() AS database_id,cc.object_id,ks.name AS constraint_schema,cc.name,
 CONVERT(varchar(40),cc.create_date,126) AS created_without_offset,
 CONVERT(varchar(40),cc.modify_date,126) AS modified_without_offset,
 t.object_id AS parent_id,s.name AS parent_schema,t.name AS parent_table,
 CONVERT(varchar(40),t.create_date,126) AS parent_created_without_offset,
 cc.parent_column_id,cc.is_disabled,cc.is_not_trusted,cc.is_not_for_replication,
 cc.is_system_named,cc.uses_database_collation,{definition}
{PARENT_RELATION} JOIN sys.schemas ks ON ks.schema_id=cc.schema_id
WHERE cc.is_ms_shipped=0 AND t.is_ms_shipped=0 ORDER BY cc.object_id OPTION(MAXDOP 1)
"#
    )
}
pub(crate) fn decode(row: &Rec, limit: usize) -> Result<MssqlCheckConstraint> {
    let narrow = |field| integer(row, field)?.try_into().map_err(|_| invalid_data());
    let database_id = narrow("database_id")?;
    let constraint = MssqlCheckConstraint {
        identity: MssqlObjectIdentity {
            database_id,
            object_id: narrow("object_id")?,
            created_without_offset: text(row, "created_without_offset")?,
        },
        schema: text(row, "constraint_schema")?,
        name: text(row, "name")?,
        modified_without_offset: text(row, "modified_without_offset")?,
        parent: MssqlObjectIdentity {
            database_id,
            object_id: narrow("parent_id")?,
            created_without_offset: text(row, "parent_created_without_offset")?,
        },
        parent_schema: text(row, "parent_schema")?,
        parent_table: text(row, "parent_table")?,
        parent_column_id: narrow("parent_column_id")?,
        definition: crate::catalog_expression::decode(row, true, "definition", limit)?.ok_or_else(invalid_data)?,
        disabled: boolean(row, "is_disabled")?,
        trusted: !boolean(row, "is_not_trusted")?,
        not_for_replication: boolean(row, "is_not_for_replication")?,
        system_named: boolean(row, "is_system_named")?,
        uses_database_collation: boolean(row, "uses_database_collation")?,
    };
    if !constraint.is_valid() {
        return Err(invalid_data());
    }
    Ok(constraint)
}
#[cfg(test)]
pub(crate) mod tests;
