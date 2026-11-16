//! Versioned columns from the existing sender/recovery/slot reads; no connection options are projected.
use super::*;

pub(crate) fn query(
    version: u32,
    standby: bool,
    database_oid: &str,
    pause_access: ReplayPauseAccess,
    limit: usize,
) -> Result<String> {
    if version < 100_000 {
        return Err(culprit_core::Error::new(
            culprit_core::ErrorCode::Unsupported,
            culprit_core::t!("collection.unsupported"),
        ));
    }
    let database = database_oid.parse::<u32>().ok().filter(|oid| *oid > 0).ok_or_else(invalid)?;
    let common = format!(
        "'server_version_num','{version}','read_database_oid','{database}','read_standby',pg_catalog.pg_is_in_recovery(),\
         'observed_at',{},'full_visibility',(SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname=current_user) \
             OR pg_catalog.pg_has_role(current_user,'pg_read_all_stats','USAGE')",
        iso("pg_catalog.clock_timestamp()")
    );
    let endpoint = if standby { recovery(version, &common, pause_access) } else { senders(&common) };
    let slots = slots(standby, database, &common);
    Ok(format!("SELECT * FROM ({endpoint} UNION ALL {slots}) native_replication LIMIT {limit}"))
}

fn senders(common: &str) -> String {
    format!(
        "SELECT 'pg_stat_replication'::text AS native_source,r.pid::text AS native_id,\
         pg_catalog.jsonb_build_object('kind','sender','name',coalesce(nullif(r.application_name,''),r.client_addr::text,r.pid::text),\
         'backend_start',{backend_start},'active_pid',r.pid::text,'slot_name',s.slot_name,'backend_xmin',r.backend_xmin::text,\
         'state',r.state,'sync_state',r.sync_state,'sync_priority',r.sync_priority::text,'database',s.database,'database_oid',s.datoid::text,\
         'logical',s.slot_type='logical','current_lsn',pg_catalog.pg_current_wal_lsn()::text,\
         'sent_lsn',r.sent_lsn::text,'write_lsn',r.write_lsn::text,'flush_lsn',r.flush_lsn::text,'replay_lsn',r.replay_lsn::text,\
         'write_lag',r.write_lag::text,'flush_lag',r.flush_lag::text,'replay_lag',r.replay_lag::text,\
         'write_lag_s',extract(epoch FROM r.write_lag)::text,'flush_lag_s',extract(epoch FROM r.flush_lag)::text,\
         'lag_s',extract(epoch FROM r.replay_lag)::text,'reply_time',{reply_time},\
         'column_availability',pg_catalog.jsonb_build_object('reply_time',pg_catalog.to_jsonb(r)?'reply_time'),\
         'behind',pg_catalog.pg_wal_lsn_diff(pg_catalog.pg_current_wal_lsn(),r.replay_lsn)::text,{common})::text AS native_values \
         FROM pg_catalog.pg_stat_replication r LEFT JOIN pg_catalog.pg_replication_slots s ON s.active_pid=r.pid",
        backend_start = iso("r.backend_start"),
        reply_time = iso("(pg_catalog.to_jsonb(r)->>'reply_time')::timestamptz"),
    )
}

fn slots(standby: bool, database: u32, common: &str) -> String {
    let position = if standby { "pg_catalog.pg_last_wal_replay_lsn()" } else { "pg_catalog.pg_current_wal_lsn()" };
    format!(
        "SELECT 'pg_replication_slots'::text AS native_source,s.slot_name::text AS native_id,\
         pg_catalog.jsonb_build_object('kind','slot','name',s.slot_name,'slot_type',s.slot_type,'active',s.active,\
         'database',s.database,'database_oid',s.datoid::text,'xmin',s.xmin::text,'catalog_xmin',s.catalog_xmin::text,\
         'slot_name',s.slot_name,'active_pid',s.active_pid::text,'backend_start',{backend_start},\
         'restart_lsn',s.restart_lsn::text,'confirmed_flush_lsn',s.confirmed_flush_lsn::text,\
         'plugin',s.plugin,'temporary',s.temporary,\
         'wal_status',pg_catalog.to_jsonb(s)->>'wal_status',\
         'safe_wal_size',pg_catalog.to_jsonb(s)->>'safe_wal_size',\
         'two_phase',pg_catalog.to_jsonb(s)->>'two_phase','two_phase_at',pg_catalog.to_jsonb(s)->>'two_phase_at',\
         'conflicting',pg_catalog.to_jsonb(s)->>'conflicting',\
         'inactive_since',{inactive_since},\
         'inactive_s',extract(epoch FROM pg_catalog.clock_timestamp()-(pg_catalog.to_jsonb(s)->>'inactive_since')::timestamptz)::text,\
         'invalidation_reason',pg_catalog.to_jsonb(s)->>'invalidation_reason',\
         'failover',pg_catalog.to_jsonb(s)->>'failover','synced',pg_catalog.to_jsonb(s)->>'synced',\
         'column_availability',pg_catalog.jsonb_build_object(\
             'wal_status',pg_catalog.to_jsonb(s)?'wal_status','safe_wal_size',pg_catalog.to_jsonb(s)?'safe_wal_size',\
             'two_phase',pg_catalog.to_jsonb(s)?'two_phase','two_phase_at',pg_catalog.to_jsonb(s)?'two_phase_at',\
             'conflicting',pg_catalog.to_jsonb(s)?'conflicting','inactive_since',pg_catalog.to_jsonb(s)?'inactive_since',\
             'invalidation_reason',pg_catalog.to_jsonb(s)?'invalidation_reason',\
             'failover',pg_catalog.to_jsonb(s)?'failover','synced',pg_catalog.to_jsonb(s)?'synced'),\
         'kept',pg_catalog.pg_wal_lsn_diff({position},s.restart_lsn)::text,{common})::text AS native_values \
         FROM pg_catalog.pg_replication_slots s LEFT JOIN pg_catalog.pg_stat_replication r ON r.pid=s.active_pid \
         WHERE s.datoid IS NULL OR s.datoid={database}",
        backend_start = iso("r.backend_start"),
        inactive_since = iso("(pg_catalog.to_jsonb(s)->>'inactive_since')::timestamptz"),
    )
}

fn recovery(version: u32, common: &str, pause_access: ReplayPauseAccess) -> String {
    // PG10-12 report received_lsn (received and flushed); PG13+ expose separate written/flushed locations.
    // https://www.postgresql.org/docs/13/monitoring-stats.html#MONITORING-PG-STAT-WAL-RECEIVER-VIEW
    let locations = if version >= 130_000 {
        "'receiver_received_lsn',NULL::text,'written_lsn',w.written_lsn::text,'flushed_lsn',w.flushed_lsn::text"
    } else {
        "'receiver_received_lsn',w.received_lsn::text,'written_lsn',NULL::text,'flushed_lsn',NULL::text"
    };
    // In PG14+ the older boolean reports a pause request, not necessarily an already paused replay process.
    // https://www.postgresql.org/docs/14/functions-admin.html#FUNCTIONS-RECOVERY-CONTROL
    let pause = pause_access.projection(version);
    format!(
        "SELECT 'pg_recovery'::text AS native_source,'local'::text AS native_id,\
         pg_catalog.jsonb_build_object('kind','standby','name','this server',\
         'receive_lsn',pg_catalog.pg_last_wal_receive_lsn()::text,'replay_lsn',pg_catalog.pg_last_wal_replay_lsn()::text,\
         'last_replay_commit_at',{last_replay},'receiver',w.status,'active_pid',w.pid::text,\
         'receive_start_lsn',w.receive_start_lsn::text,'receive_start_tli',w.receive_start_tli::text,{locations},\
         'received_tli',w.received_tli::text,\
         'last_msg_send_time',{last_send},'last_msg_receipt_time',{last_receipt},\
         'latest_end_lsn',w.latest_end_lsn::text,'latest_end_time',{latest_end},'slot_name',w.slot_name,\
         'column_availability',pg_catalog.jsonb_build_object('receiver_received_lsn',{legacy},'written_lsn',{modern},'flushed_lsn',{modern},'replay_pause_state',{pause_state}),\
         {pause},'streaming_configured',(SELECT setting<>'' FROM pg_catalog.pg_settings WHERE name='primary_conninfo'),\
         'delay_s',(SELECT setting::numeric/1000 FROM pg_catalog.pg_settings WHERE name='recovery_min_apply_delay')::text,\
         'behind',pg_catalog.pg_wal_lsn_diff(pg_catalog.pg_last_wal_receive_lsn(),pg_catalog.pg_last_wal_replay_lsn())::text,\
         'since_replay',extract(epoch FROM pg_catalog.clock_timestamp()-pg_catalog.pg_last_xact_replay_timestamp())::text,{common})::text AS native_values \
         FROM (SELECT 1) local_recovery LEFT JOIN LATERAL \
             (SELECT pid,status,receive_start_lsn,receive_start_tli,{receiver_locations},received_tli,last_msg_send_time,last_msg_receipt_time,latest_end_lsn,latest_end_time,slot_name FROM pg_catalog.pg_stat_wal_receiver LIMIT 1) w ON true",
        last_replay = iso("pg_catalog.pg_last_xact_replay_timestamp()"),
        last_send = iso("w.last_msg_send_time"),
        last_receipt = iso("w.last_msg_receipt_time"),
        latest_end = iso("w.latest_end_time"),
        legacy = version < 130_000,
        modern = version >= 130_000,
        pause_state = version >= 140_000,
        receiver_locations = if version >= 130_000 { "written_lsn,flushed_lsn" } else { "received_lsn" },
    )
}
