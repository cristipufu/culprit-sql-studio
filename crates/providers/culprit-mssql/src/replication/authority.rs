//! Native before/frame/final authority shared by the two explicitly selected Azure replica families.
use super::*;
use crate::local_source::Context;
use culprit_core::{Error, telemetry::*};
const HS_COLUMNS: &[(&str, i64, i64)] = &[
    ("replica_id", 36, 16),
    ("parent_replica_id", 36, 16),
    ("replica_server_name", 231, 256),
    ("replica_database_name", 231, 256),
    ("replica_role_desc", 231, 512),
    ("is_local", 104, 1),
    ("replica_role", 48, 1),
    ("local_database_id", 56, 4),
];
const LOCAL_COLUMNS: &[(&str, i64, i64)] = &[
    ("database_id", 56, 4),
    ("group_id", 36, 16),
    ("replica_id", 36, 16),
    ("group_database_id", 36, 16),
    ("is_local", 104, 1),
    ("is_primary_replica", 104, 1),
    ("synchronization_state", 48, 1),
    ("synchronization_state_desc", 231, 120),
    ("is_commit_participant", 104, 1),
    ("synchronization_health", 48, 1),
    ("synchronization_health_desc", 231, 120),
    ("database_state", 48, 1),
    ("database_state_desc", 231, 120),
    ("is_suspended", 104, 1),
    ("suspend_reason", 48, 1),
    ("suspend_reason_desc", 231, 120),
    ("recovery_lsn", 108, 13),
    ("truncation_lsn", 108, 13),
    ("last_sent_lsn", 108, 13),
    ("last_received_lsn", 108, 13),
    ("last_hardened_lsn", 108, 13),
    ("last_redone_lsn", 108, 13),
    ("end_of_log_lsn", 108, 13),
    ("last_commit_lsn", 108, 13),
    ("last_sent_time", 61, 8),
    ("last_received_time", 61, 8),
    ("last_hardened_time", 61, 8),
    ("last_redone_time", 61, 8),
    ("last_commit_time", 61, 8),
    ("log_send_queue_size", 127, 8),
    ("log_send_rate", 127, 8),
    ("redo_queue_size", 127, 8),
    ("redo_rate", 127, 8),
    ("filestream_send_rate", 127, 8),
    ("low_water_mark_for_ghosts", 127, 8),
    ("secondary_lag_seconds", 127, 8),
];
pub(crate) const LOCAL_STATE_COLUMNS: i64 = LOCAL_COLUMNS.len() as i64;
#[derive(Clone)]
pub(crate) struct Layout {
    pub scope: MssqlReplicationScope,
    pub observed_at: String,
    pub database_writable: bool,
    pub database_edition: String,
    pub hyperscale_columns: i64,
    pub local_state_columns: i64,
}
pub(crate) struct Witness {
    pub layout: Layout,
    pub observed_at: String,
}
impl Witness {
    pub(crate) async fn verify(&self, reader: &mut impl NativeRead, owner: &Context) -> Result<bool> {
        let Ok(after) = read(reader, owner, &self.layout.scope).await? else { return Ok(false) };
        Ok(self.layout.database_edition == after.database_edition
            && self.layout.hyperscale_columns == after.hyperscale_columns
            && self.layout.local_state_columns == after.local_state_columns
            && self.layout.database_writable == after.database_writable)
    }
}
pub(crate) fn scope(owner: &Context) -> Result<MssqlReplicationScope> {
    let scope = MssqlReplicationScope {
        database_id: owner.database_id,
        replica_id: owner.activity.replica_id.clone().ok_or_else(invalid_data)?,
    };
    if owner.activity.edition != 5 || !scope.is_valid_for(&owner.completed(owner)?) {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    Ok(scope)
}
pub(crate) async fn read(
    reader: &mut impl NativeRead,
    owner: &Context,
    scope: &MssqlReplicationScope,
) -> Result<std::result::Result<Layout, Error>> {
    let row = match native::read(reader, &layout_sql(), 1).await? {
        native::NativeRows::Available(mut rows) if rows.len() == 1 => rows.remove(0),
        native::NativeRows::Available(_) => return Err(invalid_data()),
        native::NativeRows::Unavailable(error) => return Ok(Err(error)),
    };
    let observed_at = frame(&row, owner, scope)?;
    let layout = Layout {
        scope: scope.clone(),
        observed_at,
        database_writable: owner.activity.writable.ok_or_else(invalid_data)?,
        database_edition: text(&row, "database_edition")?,
        hyperscale_columns: integer(&row, "supported_columns")?,
        local_state_columns: integer(&row, "local_state_columns")?,
    };
    Ok(Ok(layout))
}
pub(crate) fn frame(row: &Rec, owner: &Context, scope: &MssqlReplicationScope) -> Result<String> {
    let replica = crate::replica::id(row, "observer_replica_id")?.ok_or_else(invalid_data)?;
    let at = canonical_time(&text(row, "observed_at")?)?;
    if integer(row, "selected_database_id")? != i64::from(scope.database_id)
        || !replica.eq_ignore_ascii_case(&scope.replica_id)
        || crate::read::database_writable(row)? != owner.activity.writable
        || ObservationBounds::new(&owner.activity.at, &at).is_none()
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(at)
}
pub(crate) fn batch_frame(row: &Rec, owner: &Context, layout: &Layout, previous: Option<&str>) -> Result<String> {
    let at = frame(row, owner, &layout.scope)?;
    if ObservationBounds::new(&layout.observed_at, &at).is_none() || previous.is_some_and(|previous| previous != at) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(at)
}
pub(crate) async fn selected(
    reader: &mut impl NativeRead,
    owner: &Context,
    scope: Option<&MssqlReplicationScope>,
    remaining: usize,
) -> Result<(Vec<selection::Part>, Option<Witness>)> {
    let mut parts = Vec::new();
    let Some(scope) = scope else { return Ok((unavailable(CollectionStop::SourceUnavailable.error()), None)) };
    if remaining == 0 {
        return Ok((unavailable(CollectionStop::RowLimit.error()), None));
    }
    let layout = match read(reader, owner, scope).await? {
        Ok(layout) => layout,
        Err(error) => return Ok((unavailable(error), None)),
    };
    let (topology, topology_at) = hyperscale::read_with_authority(reader, owner, &layout, remaining).await?;
    let count = match &topology {
        selection::Part::Observed { facts, .. } => facts.len(),
        _ => 0,
    };
    parts.push(topology);
    let (state, state_at) = azure_state::read(reader, owner, &layout, remaining.saturating_sub(count)).await?;
    parts.push(state);
    if topology_at
        .as_deref()
        .zip(state_at.as_deref())
        .is_some_and(|(topology, state)| ObservationBounds::new(topology, state).is_none())
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    let at = state_at.or(topology_at);
    Ok((parts, at.map(|observed_at| Witness { layout, observed_at })))
}
fn unavailable(error: Error) -> Vec<selection::Part> {
    vec![
        selection::Part::Unavailable { selection: Selection::Hyperscale, error: error.clone() },
        selection::Part::Unavailable { selection: Selection::AzureLocalState, error },
    ]
}

pub(crate) fn local_projection() -> String {
    LOCAL_COLUMNS.iter().map(|(name, _, _)| *name).collect::<Vec<_>>().join(",")
}

fn layout_sql() -> String {
    let replica = crate::replica::ID_EXPRESSION;
    let hyperscale = count("sys.dm_hs_database_replicas", HS_COLUMNS);
    let local = count("sys.dm_database_replica_states", LOCAL_COLUMNS);
    format!(
        r#"SELECT /* culprit */ DB_ID() AS selected_database_id,{replica} AS observer_replica_id,
 CONVERT(nvarchar(128),DATABASEPROPERTYEX(DB_NAME(),N'Edition')) AS database_edition,
 CONVERT(nvarchar(60),DATABASEPROPERTYEX(DB_NAME(),N'Updateability')) AS updateability,
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,
 {hyperscale} AS supported_columns,{local} AS local_state_columns"#
    )
}
fn count(object: &str, columns: &[(&str, i64, i64)]) -> String {
    let predicates = columns
        .iter()
        .map(|(name, kind, length)| {
            let precision = if *kind == 108 { " AND precision=25 AND scale=0" } else { "" };
            format!("(name=N'{name}' AND system_type_id={kind} AND max_length={length}{precision})")
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    format!("(SELECT COUNT(*) FROM sys.all_columns WHERE object_id=OBJECT_ID(N'{object}') AND ({predicates}))")
}
#[cfg(test)]
pub(crate) mod tests;
