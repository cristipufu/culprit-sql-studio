//! Explicit selected Hyperscale topology through the replication reader. No ordinary-scan acquisition.
use super::{native, selection::Part, *};
use crate::{local_source::Context, read::optional_boolean};
use culprit_core::{Error, ErrorCode, telemetry::*};

pub(crate) const MAX_TOPOLOGY_ROWS: usize = 64;
const NATIVE_COLUMNS: i64 = 8;
#[cfg(test)]
pub(crate) async fn read(
    reader: &mut impl NativeRead,
    owner: &Context,
    limit: usize,
) -> Result<(Part, Option<super::authority::Witness>)> {
    let scope = super::authority::scope(owner).ok();
    let (parts, witness) = super::authority::selected(reader, owner, scope.as_ref(), limit).await?;
    let first = parts.into_iter().next().ok_or_else(invalid_data)?;
    Ok((first, witness))
}
pub(crate) async fn read_with_authority(
    reader: &mut impl NativeRead,
    owner: &Context,
    layout: &super::authority::Layout,
    limit: usize,
) -> Result<(Part, Option<String>)> {
    if limit == 0 {
        return Ok(unavailable(CollectionStop::RowLimit.error()));
    }
    if layout.database_edition != "Hyperscale" {
        return Ok((Part::NotApplicable { selection: Selection::Hyperscale }, None));
    }
    if layout.hyperscale_columns != NATIVE_COLUMNS {
        return Ok(unavailable(Error::new(
            ErrorCode::Unsupported,
            culprit_core::t!("provider.mssql.replication.native_unavailable"),
        )));
    }
    let scope = &layout.scope;
    let limit = limit.min(MAX_TOPOLOGY_ROWS);
    let rows = match native::read(reader, &sql(limit + 1, scope.database_id), limit + 1).await? {
        native::NativeRows::Available(rows) => rows,
        native::NativeRows::Unavailable(error) => return Ok(unavailable(error)),
    };
    if rows.is_empty() {
        return Err(invalid_data());
    }
    let mut facts = Vec::new();
    let mut keys = BTreeSet::new();
    let mut at = None;
    for row in &rows {
        let current = super::authority::batch_frame(row, owner, layout, at.as_deref())?;
        at = Some(current.clone());
        if !crate::read::boolean(row, "topology_present")? {
            if rows.len() != 1
                || [
                    "replica_id",
                    "replica_server_name",
                    "replica_database_name",
                    "is_local",
                    "replica_role",
                    "native_role",
                    "local_database_id",
                    "parent_replica_id",
                ]
                .into_iter()
                .any(|key| !matches!(row.columns().find(|(name, _)| *name == key), Some((_, crate::value::Val::Null))))
            {
                return Err(invalid_data());
            }
            continue;
        }
        let point = MssqlHyperscaleReplica {
            selected_database_id: scope.database_id,
            observer_replica_id: scope.replica_id.clone(),
            database_writable: owner.activity.writable.ok_or_else(invalid_data)?,
            observed_at: current,
            replica_id: crate::replica::id(row, "replica_id")?.ok_or_else(invalid_data)?,
            replica_server_name: optional_text(row, "replica_server_name")?,
            replica_database_name: optional_text(row, "replica_database_name")?,
            is_local: optional_boolean(row, "is_local")?,
            replica_role: native_code(row, "replica_role")?,
            native_role: optional_text(row, "native_role")?,
            local_database_id: optional_integer(row, "local_database_id")?
                .map(|value| u32::try_from(value).map_err(|_| invalid_data()))
                .transpose()?,
            parent_replica_id: crate::replica::id(row, "parent_replica_id")?,
        };
        let fact = ReplicationFact::HyperscaleReplica(Box::new(point));
        if !fact.is_valid() || !keys.insert(fact.key()) {
            return Err(invalid_data());
        }
        facts.push(fact);
    }
    let limited = facts.len() > limit;
    facts.truncate(limit);
    let part = Part::Observed { selection: Selection::Hyperscale, facts, complete: false, limited };
    Ok((part, at))
}
fn unavailable(error: Error) -> (Part, Option<String>) {
    (Part::Unavailable { selection: Selection::Hyperscale, error }, None)
}
fn sql(limit: usize, database_id: u32) -> String {
    // Explicit DB_ID only. NULL would widen a pooled invocation. TOP bounds retained rows, not internal work.
    // https://learn.microsoft.com/sql/relational-databases/system-functions/sys-dm-hs-database-replicas
    let replica = crate::replica::ID_EXPRESSION;
    format!(
        r#"DECLARE /* culprit */ @selected_database_id int=DB_ID(),@at datetime2(7)=SYSUTCDATETIME(),
 @replica uniqueidentifier={replica},
 @updateability nvarchar(60)=CONVERT(nvarchar(60),DATABASEPROPERTYEX(DB_NAME(),N'Updateability'));
 IF @selected_database_id IS NULL OR @selected_database_id<>{database_id} THROW 50000,N'culprit',1;
 SELECT /* culprit */ @selected_database_id AS selected_database_id,CONVERT(varchar(36),@replica) AS observer_replica_id,
 @updateability AS updateability,CONVERT(varchar(40),@at,127)+'Z' AS observed_at,
 CASE WHEN r.replica_id IS NULL THEN 0 ELSE 1 END AS topology_present,
 CONVERT(varchar(36),r.replica_id) AS replica_id,r.replica_server_name,r.replica_database_name,r.is_local,
 r.replica_role,r.replica_role_desc AS native_role,r.local_database_id,CONVERT(varchar(36),r.parent_replica_id) AS parent_replica_id
 FROM (VALUES(1)) anchor(v) OUTER APPLY(SELECT TOP({limit}) replica_id,replica_server_name,replica_database_name,is_local,
 replica_role,replica_role_desc,local_database_id,parent_replica_id
 FROM sys.dm_hs_database_replicas(@selected_database_id)) r OPTION(MAXDOP 1)"#
    )
}

#[cfg(test)]
pub(crate) mod tests;
