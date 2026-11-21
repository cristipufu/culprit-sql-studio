//! Exact local state only. The native DMV is locally relevant, possibly stale and not a topology inventory.
use super::{authority, native, selection::Part, *};
use crate::{local_source::Context, read::boolean};
use culprit_core::{Error, ErrorCode, telemetry::*};
const MAX_LOCAL_ROWS: usize = 1;
const LOOKAHEAD_ROWS: usize = MAX_LOCAL_ROWS + 1;
pub(crate) async fn read(
    reader: &mut impl NativeRead,
    owner: &Context,
    layout: &authority::Layout,
    limit: usize,
) -> Result<(Part, Option<String>)> {
    if limit == 0 {
        return Ok((
            Part::Unavailable { selection: Selection::AzureLocalState, error: CollectionStop::RowLimit.error() },
            None,
        ));
    }
    if layout.local_state_columns != authority::LOCAL_STATE_COLUMNS {
        return Ok((
            Part::Unavailable {
                selection: Selection::AzureLocalState,
                error: Error::new(
                    ErrorCode::Unsupported,
                    culprit_core::t!("provider.mssql.replication.native_unavailable"),
                ),
            },
            None,
        ));
    }
    let rows = match native::read(reader, &sql(layout.scope.database_id), LOOKAHEAD_ROWS).await? {
        native::NativeRows::Available(rows) => rows,
        native::NativeRows::Unavailable(error) => {
            return Ok((Part::Unavailable { selection: Selection::AzureLocalState, error }, None));
        }
    };
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    let at = authority::batch_frame(row, owner, layout, None)?;
    let mut facts = Vec::new();
    if boolean(row, "state_present")? {
        let point = MssqlAzureReplicaState {
            database_id: integer(row, "database_id")?.try_into().map_err(|_| invalid_data())?,
            replica_id: crate::replica::id(row, "replica_id")?.ok_or_else(invalid_data)?,
            is_local: boolean(row, "is_local")?,
            database_writable: layout.database_writable,
            observed_at: at.clone(),
            group_id: optional_text(row, "group_id")?,
            group_database_id: optional_text(row, "group_database_id")?,
            progress: fields::database_progress(row)?,
        };
        let context = owner.completed(owner)?;
        // Frame time follows the original start; upper bound is confirmed by the source's existing after read.
        let context = NativeReadContext { observed_until: at.clone(), ..context };
        if !point.fits_read(&layout.scope, &context) {
            return Err(CollectionStop::SourceChanged.error());
        }
        facts.push(ReplicationFact::AzureReplicaState(Box::new(point)));
    } else if row.columns().any(|(name, value)| {
        !matches!(
            name,
            "selected_database_id" | "observer_replica_id" | "updateability" | "observed_at" | "state_present"
        ) && !matches!(value, crate::value::Val::Null)
    }) {
        return Err(invalid_data());
    }
    Ok((Part::Observed { selection: Selection::AzureLocalState, facts, complete: false, limited: false }, Some(at)))
}
fn sql(database_id: u32) -> String {
    let replica = crate::replica::ID_EXPRESSION;
    let columns = authority::local_projection();
    // No pool enumeration, quorum fields, row reads or catalog joins. TOP2 is an ambiguity witness, not a server-work bound.
    format!(
        r#"DECLARE /* culprit */ @database_id int=DB_ID(),@observer uniqueidentifier={replica},
 @at datetime2(7)=SYSUTCDATETIME(),@updateability nvarchar(60)=CONVERT(nvarchar(60),DATABASEPROPERTYEX(DB_NAME(),N'Updateability'));
 IF @database_id IS NULL OR @database_id<>{database_id} THROW 50000,N'culprit',1;
 SELECT /* culprit */ @database_id AS selected_database_id,CONVERT(varchar(36),@observer) AS observer_replica_id,
 @updateability AS updateability,CONVERT(varchar(40),@at,127)+'Z' AS observed_at,
 CASE WHEN d.replica_id IS NULL THEN 0 ELSE 1 END AS state_present,
 d.database_id,d.group_id,d.group_database_id,d.replica_id,d.is_local,d.is_primary_replica AS is_primary,
 d.synchronization_state AS state,d.synchronization_state_desc AS native_state,
 CAST(NULL AS tinyint) AS mode,CAST(NULL AS nvarchar(60)) AS native_mode,
 d.is_commit_participant,d.synchronization_health,d.synchronization_health_desc AS native_synchronization_health,
 d.database_state,d.database_state_desc AS native_database_state,d.is_suspended,d.suspend_reason,d.suspend_reason_desc AS native_suspend_reason,
 d.log_send_queue_size AS log_send_queue_kb,d.log_send_rate AS log_send_rate_kb_per_second,
 d.redo_queue_size AS redo_queue_kb,d.redo_rate AS redo_rate_kb_per_second,d.secondary_lag_seconds,
 d.filestream_send_rate AS filestream_send_rate_kb_per_second,d.low_water_mark_for_ghosts,
 CONVERT(varchar(25),d.recovery_lsn) AS recovery_lsn,CONVERT(varchar(25),d.truncation_lsn) AS truncation_lsn,
 CONVERT(varchar(25),d.last_sent_lsn) AS last_sent_lsn,CONVERT(varchar(25),d.last_received_lsn) AS last_received_lsn,
 CONVERT(varchar(25),d.last_hardened_lsn) AS last_hardened_lsn,CONVERT(varchar(25),d.last_redone_lsn) AS last_redone_lsn,
 CONVERT(varchar(25),d.end_of_log_lsn) AS end_of_log_lsn,CONVERT(varchar(25),d.last_commit_lsn) AS last_commit_lsn,
 CONVERT(varchar(40),d.last_sent_time,126) AS last_sent_without_offset,
 CONVERT(varchar(40),d.last_received_time,126) AS last_received_without_offset,
 CONVERT(varchar(40),d.last_hardened_time,126) AS last_hardened_without_offset,
 CONVERT(varchar(40),d.last_redone_time,126) AS last_redone_without_offset,
 CONVERT(varchar(40),d.last_commit_time,126) AS last_commit_without_offset
 FROM (VALUES(1)) a(v) OUTER APPLY(SELECT TOP({LOOKAHEAD_ROWS}) {columns} FROM sys.dm_database_replica_states
 WHERE database_id=@database_id AND is_local=1 AND replica_id=@observer) d OPTION(MAXDOP 1)"#
    )
}
#[cfg(test)]
pub(crate) mod tests;
