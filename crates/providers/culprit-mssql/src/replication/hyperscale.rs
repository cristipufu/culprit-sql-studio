//! Explicit selected Hyperscale topology through the replication reader. No ordinary-scan acquisition.
use super::{native, selection::Part, *};
use crate::{local_source::Context, read::optional_boolean};
use culprit_core::{Error, ErrorCode, telemetry::*};

pub(crate) const MAX_TOPOLOGY_ROWS: usize = 64;
const NATIVE_COLUMNS: i64 = 8;
fn layout_sql() -> String {
    let replica = crate::replica::ID_EXPRESSION;
    format!(
        r#"SELECT /* culprit */ DB_ID() AS selected_database_id,
 {replica} AS observer_replica_id,
 CONVERT(nvarchar(128),DATABASEPROPERTYEX(DB_NAME(),N'Edition')) AS database_edition,
 CONVERT(nvarchar(60),DATABASEPROPERTYEX(DB_NAME(),N'Updateability')) AS updateability,
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,
 (SELECT COUNT(*) FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.dm_hs_database_replicas') AND
 ((name IN(N'replica_id',N'parent_replica_id') AND system_type_id=36 AND max_length=16)
 OR(name IN(N'replica_server_name',N'replica_database_name') AND system_type_id=231 AND max_length=256)
 OR(name=N'replica_role_desc' AND system_type_id=231 AND max_length=512)
 OR(name=N'is_local' AND system_type_id=104 AND max_length=1) OR(name=N'replica_role' AND system_type_id=48 AND max_length=1)
 OR(name=N'local_database_id' AND system_type_id=56 AND max_length=4))) AS supported_columns"#
    )
}

#[derive(Clone)]
pub(crate) struct Witness {
    pub scope: MssqlReplicationScope,
    pub observed_at: String,
    writable: bool,
}
impl Witness {
    pub(crate) async fn verify<R: NativeRead>(&self, reader: &mut R, owner: &Context, until: &str) -> Result<bool> {
        let Ok(row) = metadata(reader).await? else { return Ok(false) };
        let at = validate_witness(&row, owner, &self.scope)?;
        Ok(text(&row, "database_edition")? == "Hyperscale"
            && integer(&row, "supported_columns")? == NATIVE_COLUMNS
            && crate::read::database_writable(&row)? == Some(self.writable)
            && ObservationBounds::new(until, &at).is_some())
    }
}
pub(crate) fn scope(owner: &Context) -> Result<MssqlReplicationScope> {
    let scope = MssqlReplicationScope {
        database_id: owner.database_id,
        replica_id: owner.activity.replica_id.clone().ok_or_else(invalid_data)?,
    };
    let context = owner.completed(owner)?;
    if owner.activity.edition != 5 || !scope.is_valid_for(&context) {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    Ok(scope)
}
async fn metadata(reader: &mut impl NativeRead) -> Result<std::result::Result<Rec, Error>> {
    match native::read(reader, &layout_sql(), 1).await? {
        native::NativeRows::Available(mut rows) if rows.len() == 1 => Ok(Ok(rows.remove(0))),
        native::NativeRows::Available(_) => Err(invalid_data()),
        native::NativeRows::Unavailable(error) => Ok(Err(error)),
    }
}
fn validate_witness(row: &Rec, owner: &Context, scope: &MssqlReplicationScope) -> Result<String> {
    let database = integer(row, "selected_database_id")?;
    let replica = crate::replica::id(row, "observer_replica_id")?.ok_or_else(invalid_data)?;
    let at = canonical_time(&text(row, "observed_at")?)?;
    if database != i64::from(scope.database_id)
        || !replica.eq_ignore_ascii_case(&scope.replica_id)
        || crate::read::database_writable(row)? != owner.activity.writable
        || ObservationBounds::new(&owner.activity.at, &at).is_none()
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(at)
}
pub(crate) async fn read(
    reader: &mut impl NativeRead,
    owner: &Context,
    limit: usize,
) -> Result<(Part, Option<Witness>)> {
    let scope = match scope(owner) {
        Ok(scope) => scope,
        Err(error) => return Ok(unavailable(error)),
    };
    if limit == 0 {
        return Ok(unavailable(CollectionStop::RowLimit.error()));
    }
    let row = match metadata(reader).await? {
        Ok(row) => row,
        Err(error) => return Ok(unavailable(error)),
    };
    let first_at = validate_witness(&row, owner, &scope)?;
    if text(&row, "database_edition")? != "Hyperscale" {
        return Ok((Part::NotApplicable { selection: Selection::Hyperscale }, None));
    }
    if integer(&row, "supported_columns")? != NATIVE_COLUMNS {
        return Ok(unavailable(Error::new(
            ErrorCode::Unsupported,
            culprit_core::t!("provider.mssql.replication.native_unavailable"),
        )));
    }
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
        let current = validate_witness(row, owner, &scope)?;
        if ObservationBounds::new(&first_at, &current).is_none() || at.as_ref().is_some_and(|at| at != &current) {
            return Err(CollectionStop::SourceChanged.error());
        }
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
    let witness = Witness {
        scope,
        observed_at: at.ok_or_else(invalid_data)?,
        writable: owner.activity.writable.ok_or_else(invalid_data)?,
    };
    Ok((part, Some(witness)))
}
fn unavailable(error: Error) -> (Part, Option<Witness>) {
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
