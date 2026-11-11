//! Native foreign keys retain ordered column identities and flags before projection into app relationships.
use crate::{
    read::{NativeRead, Parameter, ReadError, boolean, integer, invalid_data, text},
    value::Rec,
};
use culprit_core::{ForeignKey, Result, telemetry::*};

const MAX_APPLICATION_KEYS: usize = 10_000;
pub(crate) const PARENT_RELATION: &str = "FROM sys.foreign_keys fk JOIN sys.tables t ON t.object_id=fk.parent_object_id JOIN sys.schemas s ON s.schema_id=t.schema_id";
pub(crate) fn issues_sql() -> String {
    format!(
        "SELECT s.name AS schema_name,t.name AS table_name,fk.name,0 AS kind,CAST(fk.is_disabled AS int) AS disabled {PARENT_RELATION} WHERE (fk.is_not_trusted=1 OR fk.is_disabled=1) AND fk.is_not_for_replication=0 AND fk.is_ms_shipped=0"
    )
}
pub(crate) fn select_sql() -> String {
    format!(
        r#"
SELECT TOP (@P1) DB_ID() AS database_id,fk.object_id,fk.name,ks.name AS constraint_schema,
 CONVERT(varchar(40),fk.create_date,126) AS created_without_offset,
 CONVERT(varchar(40),fk.modify_date,126) AS modified_without_offset,
 t.object_id AS parent_id,s.name AS parent_schema,t.name AS parent_table,
 CONVERT(varchar(40),t.create_date,126) AS parent_created_without_offset,
 rt.object_id AS referenced_id,rs.name AS referenced_schema,rt.name AS referenced_table,
 CONVERT(varchar(40),rt.create_date,126) AS referenced_created_without_offset,
 fk.delete_referential_action AS delete_action,fk.update_referential_action AS update_action,
 fk.is_disabled,fk.is_not_trusted,fk.is_not_for_replication,fk.is_system_named,
 (SELECT fkc.constraint_column_id AS ordinal,c.column_id AS parent_column_id,c.name AS parent_column,
         rc.column_id AS referenced_column_id,rc.name AS referenced_column
  FROM sys.foreign_key_columns fkc
  JOIN sys.columns c ON c.object_id=fkc.parent_object_id AND c.column_id=fkc.parent_column_id
  JOIN sys.columns rc ON rc.object_id=fkc.referenced_object_id AND rc.column_id=fkc.referenced_column_id
  WHERE fkc.constraint_object_id=fk.object_id ORDER BY fkc.constraint_column_id FOR JSON PATH) AS column_pairs
{PARENT_RELATION}
JOIN sys.schemas ks ON ks.schema_id=fk.schema_id
JOIN sys.tables rt ON rt.object_id=fk.referenced_object_id
JOIN sys.schemas rs ON rs.schema_id=rt.schema_id
WHERE fk.is_ms_shipped=0 AND t.is_ms_shipped=0
ORDER BY fk.object_id OPTION(MAXDOP 1)
"#
    )
}
fn action(value: i64) -> Result<MssqlForeignKeyAction> {
    match value {
        0 => Ok(MssqlForeignKeyAction::NoAction),
        1 => Ok(MssqlForeignKeyAction::Cascade),
        2 => Ok(MssqlForeignKeyAction::SetNull),
        3 => Ok(MssqlForeignKeyAction::SetDefault),
        _ => Err(invalid_data()),
    }
}
pub(crate) fn decode(row: &Rec) -> Result<MssqlForeignKey> {
    let database_id = integer(row, "database_id")?.try_into().map_err(|_| invalid_data())?;
    let identity = |object_id, created| -> Result<MssqlObjectIdentity> {
        Ok(MssqlObjectIdentity {
            database_id,
            object_id: integer(row, object_id)?.try_into().map_err(|_| invalid_data())?,
            created_without_offset: text(row, created)?,
        })
    };
    let columns = serde_json::from_str(&text(row, "column_pairs")?).map_err(|_| invalid_data())?;
    let key = MssqlForeignKey {
        identity: identity("object_id", "created_without_offset")?,
        schema: text(row, "constraint_schema")?,
        name: text(row, "name")?,
        modified_without_offset: text(row, "modified_without_offset")?,
        parent: identity("parent_id", "parent_created_without_offset")?,
        parent_schema: text(row, "parent_schema")?,
        parent_table: text(row, "parent_table")?,
        referenced: identity("referenced_id", "referenced_created_without_offset")?,
        referenced_schema: text(row, "referenced_schema")?,
        referenced_table: text(row, "referenced_table")?,
        columns,
        delete_action: action(integer(row, "delete_action")?)?,
        update_action: action(integer(row, "update_action")?)?,
        disabled: boolean(row, "is_disabled")?,
        trusted: !boolean(row, "is_not_trusted")?,
        not_for_replication: boolean(row, "is_not_for_replication")?,
        system_named: boolean(row, "is_system_named")?,
    };
    if !key.is_valid() {
        return Err(invalid_data());
    }
    Ok(key)
}
pub(crate) async fn application<R: NativeRead>(reader: &mut R) -> Result<Vec<ForeignKey>> {
    let rows = reader
        .query(&select_sql(), vec![Parameter::Integer((MAX_APPLICATION_KEYS + 1) as i64)], MAX_APPLICATION_KEYS + 1)
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > MAX_APPLICATION_KEYS {
        return Err(CollectionStop::RowLimit.error());
    }
    rows.iter().map(|row| decode(row).map(project)).collect()
}
fn project(key: MssqlForeignKey) -> ForeignKey {
    ForeignKey {
        name: key.name,
        schema: key.parent_schema,
        table: key.parent_table,
        columns: key.columns.iter().map(|column| column.parent_column.clone()).collect(),
        ref_schema: key.referenced_schema,
        ref_table: key.referenced_table,
        ref_columns: key.columns.into_iter().map(|column| column.referenced_column).collect(),
        delete_rule: match key.delete_action {
            MssqlForeignKeyAction::NoAction => None,
            MssqlForeignKeyAction::Cascade => Some("CASCADE".into()),
            MssqlForeignKeyAction::SetNull => Some("SET NULL".into()),
            MssqlForeignKeyAction::SetDefault => Some("SET DEFAULT".into()),
        },
    }
}

#[cfg(test)]
pub(crate) mod tests;
