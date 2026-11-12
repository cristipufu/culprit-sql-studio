//! One bounded native trigger reader for app metadata and background definitions. No trigger body is executed.
use crate::{
    read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_boolean, text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};

const MAX_APPLICATION_TRIGGERS: usize = 20_000;
#[derive(Clone, Copy)]
pub(crate) enum Selection {
    ActiveTables,
    AllDml,
}
pub(crate) fn select_sql(selection: Selection) -> String {
    let predicate = match selection {
        Selection::ActiveTables => "AND tr.is_disabled=0 AND parent.type='U'",
        Selection::AllDml => "",
    };
    format!(
        r#"SELECT TOP (@P1) DB_ID() AS database_id,tr.object_id,tr.parent_id,
 CONVERT(varchar(40),tr.create_date,126) AS created_without_offset,
 CONVERT(varchar(40),tr.modify_date,126) AS modified_without_offset,
 CONVERT(varchar(40),parent.create_date,126) AS parent_created_without_offset,
 ts.name AS schema_name,tr.name,ps.name AS parent_schema,parent.name AS parent_name,
 RTRIM(parent.type) AS parent_type,RTRIM(tr.type) AS native_type,
 tr.is_disabled,tr.is_instead_of_trigger,tr.is_not_for_replication,
 CAST(OBJECTPROPERTY(tr.object_id,'ExecIsInsertTrigger') AS int) AS insert_event,
 CAST(OBJECTPROPERTY(tr.object_id,'ExecIsDeleteTrigger') AS int) AS delete_event,
 CAST(OBJECTPROPERTY(tr.object_id,'ExecIsUpdateTrigger') AS int) AS update_event
FROM sys.triggers tr JOIN sys.objects o ON o.object_id=tr.object_id
JOIN sys.schemas ts ON ts.schema_id=o.schema_id
JOIN sys.objects parent ON parent.object_id=tr.parent_id
JOIN sys.schemas ps ON ps.schema_id=parent.schema_id
WHERE tr.is_ms_shipped=0 AND tr.parent_class=1 AND parent.type IN('U','V') {predicate}
ORDER BY tr.object_id OPTION(MAXDOP 1)"#
    )
}
pub(crate) fn decode(row: &Rec) -> Result<MssqlDmlTrigger> {
    let database_id = integer(row, "database_id")?.try_into().map_err(|_| invalid_data())?;
    let required_bool = |name| optional_boolean(row, name)?.ok_or_else(invalid_data);
    let value = MssqlDmlTrigger {
        identity: MssqlObjectIdentity {
            database_id,
            object_id: integer(row, "object_id")?.try_into().map_err(|_| invalid_data())?,
            created_without_offset: crate::sql_time::native_time(&text(row, "created_without_offset")?)?,
        },
        parent: MssqlObjectIdentity {
            database_id,
            object_id: integer(row, "parent_id")?.try_into().map_err(|_| invalid_data())?,
            created_without_offset: crate::sql_time::native_time(&text(row, "parent_created_without_offset")?)?,
        },
        schema: text(row, "schema_name")?,
        name: text(row, "name")?,
        parent_schema: text(row, "parent_schema")?,
        parent_name: text(row, "parent_name")?,
        parent_type: text(row, "parent_type")?,
        native_type: text(row, "native_type")?,
        modified_without_offset: crate::sql_time::native_time(&text(row, "modified_without_offset")?)?,
        disabled: required_bool("is_disabled")?,
        instead_of: required_bool("is_instead_of_trigger")?,
        not_for_replication: required_bool("is_not_for_replication")?,
        insert_event: optional_boolean(row, "insert_event")?,
        delete_event: optional_boolean(row, "delete_event")?,
        update_event: optional_boolean(row, "update_event")?,
    };
    if !value.is_valid() {
        return Err(invalid_data());
    }
    Ok(value)
}
pub(crate) async fn application(reader: &mut impl NativeRead) -> Result<Vec<MssqlDmlTrigger>> {
    let rows = reader
        .query(
            &select_sql(Selection::ActiveTables),
            vec![Parameter::Integer((MAX_APPLICATION_TRIGGERS + 1) as i64)],
            MAX_APPLICATION_TRIGGERS + 1,
        )
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > MAX_APPLICATION_TRIGGERS {
        return Err(CollectionStop::RowLimit.error());
    }
    let triggers = rows.iter().map(decode).collect::<Result<Vec<_>>>()?;
    if triggers.iter().any(|trigger| trigger.disabled || trigger.parent_type != "U") {
        return Err(invalid_data());
    }
    Ok(triggers)
}
pub(crate) fn event_codes(trigger: &MssqlDmlTrigger) -> String {
    [('I', trigger.insert_event), ('D', trigger.delete_event), ('U', trigger.update_event)]
        .into_iter()
        .filter_map(|(code, present)| (present == Some(true)).then_some(code))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests;
