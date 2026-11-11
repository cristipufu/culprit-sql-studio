//! Changes SQL Server dates itself: `sys.objects` keeps when each table, view, procedure, function and trigger was
//! created and last altered (a table's date also moves when one of its indexes is created or altered). Catalog only.

use culprit_core::changes::epoch;
use culprit_core::i18n::Message;
use culprit_core::{ChangeEvent, EventAction, EventKind, EventSource, message};

use crate::read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_text, text};
use crate::sql_time::utc_local_iso;
use crate::value::Rec;
use culprit_core::{
    Result,
    telemetry::{MssqlCatalogObject, MssqlObjectIdentity},
};

/// Objects listed per scan, newest change first.
const MAX_OBJECTS: usize = 300;

pub(crate) fn select_sql(changed_since: bool) -> String {
    let (estimates, predicate) = if changed_since {
        (
            format!(",{} AS created,{} AS modified", utc_local_iso("o.create_date"), utc_local_iso("o.modify_date")),
            "AND DATEADD(minute,DATEDIFF(minute,GETDATE(),GETUTCDATE()),o.modify_date)>CAST(@P2 AS datetime2(0))",
        )
    } else {
        (String::new(), "")
    };
    format!(
        r#"SELECT TOP (@P1) DB_ID() AS database_id,o.object_id,
 s.name AS schema_name,o.name,RTRIM(o.type) AS native_type,
 CONVERT(varchar(40),o.create_date,126) AS created_without_offset,
 CONVERT(varchar(40),o.modify_date,126) AS modified_without_offset,
 o.parent_object_id,OBJECT_SCHEMA_NAME(o.parent_object_id) AS parent_schema,OBJECT_NAME(o.parent_object_id) AS parent_name
 {estimates}
FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id
WHERE o.is_ms_shipped=0 AND o.type IN('U','V','P','FN','IF','TF','TR') {predicate}
ORDER BY o.modify_date DESC,o.object_id OPTION(MAXDOP 1)"#
    )
}
pub(crate) fn decode(row: &Rec) -> Result<MssqlCatalogObject> {
    let parent = integer(row, "parent_object_id")?;
    let value = MssqlCatalogObject {
        identity: MssqlObjectIdentity {
            database_id: integer(row, "database_id")?.try_into().map_err(|_| invalid_data())?,
            object_id: integer(row, "object_id")?.try_into().map_err(|_| invalid_data())?,
            created_without_offset: crate::sql_time::native_time(&text(row, "created_without_offset")?)?,
        },
        schema: text(row, "schema_name")?,
        name: text(row, "name")?,
        native_type: text(row, "native_type")?,
        modified_without_offset: crate::sql_time::native_time(&text(row, "modified_without_offset")?)?,
        parent_object_id: (parent != 0).then(|| parent.try_into().map_err(|_| invalid_data())).transpose()?,
        parent_schema: optional_text(row, "parent_schema")?,
        parent_name: optional_text(row, "parent_name")?,
    };
    if !value.is_valid() {
        return Err(invalid_data());
    }
    Ok(value)
}
pub(crate) async fn changes(client: &mut impl NativeRead, since: &str) -> Result<Vec<ChangeEvent>> {
    // These UTC values retain the app's existing current-offset estimate; export keeps native local dates instead.
    let Some(since) = culprit_core::changes::sql_timestamp(since) else { return Ok(Vec::new()) };
    let rows = client
        .query(
            &select_sql(true),
            vec![Parameter::Integer(MAX_OBJECTS as i64), Parameter::Text(since.clone())],
            MAX_OBJECTS + 1,
        )
        .await
        .map_err(ReadError::into_error)?;
    let since = epoch(&format!("{}Z", since.replace(' ', "T"))).ok_or_else(invalid_data)?;
    let mut out = Vec::new();
    for r in rows {
        let object = decode(&r)?;
        let (schema, name, kind) = (object.schema, object.name, object.native_type);
        let qualified = format!("{schema}.{name}");
        let (created, modified) = (Some(text(&r, "created")?), Some(text(&r, "modified")?));
        let new = created.as_deref().and_then(epoch).filter(|t| *t > since);
        let altered = modified.as_deref().and_then(epoch).filter(|m| new.is_none_or(|c| *m > c + 2.0));
        let parent = object.parent_schema.zip(object.parent_name).map(|(schema, name)| format!("{schema}.{name}"));
        let event = |at: &str, action: EventAction| {
            let title = title(&kind, action, &name, &qualified, parent.as_deref());
            let e = ChangeEvent::new(
                if kind == "U" { EventKind::Table } else { EventKind::Code },
                EventSource::Database,
                &qualified,
                at,
                title,
            )
            .action(action);
            match (kind.as_str(), &parent) {
                ("U", _) => e.table(qualified.clone()),
                ("TR", Some(table)) => e.table(table.clone()).object(qualified.clone()),
                _ => e.object(qualified.clone()),
            }
        };
        if new.is_some()
            && let Some(at) = &created
        {
            out.push(event(at, EventAction::Created));
        }
        if altered.is_some()
            && let Some(at) = &modified
        {
            out.push(event(at, EventAction::Changed));
        }
    }
    Ok(out)
}

/// How a change to an object reads: `kind` is its sys.objects type, `parent` the table a trigger is on.
pub(crate) fn title(kind: &str, action: EventAction, name: &str, qualified: &str, parent: Option<&str>) -> Message {
    let created = action == EventAction::Created;
    match (kind, parent) {
        ("U", _) if created => message!("provider.mssql.change.table.created", name = qualified),
        ("U", _) => message!("provider.mssql.change.table.altered", name = qualified),
        ("V", _) if created => message!("provider.mssql.change.view.created", name = qualified),
        ("V", _) => message!("provider.mssql.change.view.altered", name = qualified),
        ("P", _) if created => message!("provider.mssql.change.procedure.created", name = qualified),
        ("P", _) => message!("provider.mssql.change.procedure.altered", name = qualified),
        ("TR", Some(table)) if created => {
            message!("provider.mssql.change.trigger_on.created", name = name, table = table)
        }
        ("TR", Some(table)) => message!("provider.mssql.change.trigger_on.altered", name = name, table = table),
        ("TR", None) if created => message!("provider.mssql.change.trigger.created", name = qualified),
        ("TR", None) => message!("provider.mssql.change.trigger.altered", name = qualified),
        _ if created => message!("provider.mssql.change.function.created", name = qualified),
        _ => message!("provider.mssql.change.function.altered", name = qualified),
    }
}

#[cfg(test)]
pub(crate) mod tests;
