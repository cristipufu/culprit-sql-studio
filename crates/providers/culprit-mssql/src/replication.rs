//! One bounded native reader serves interactive replication advice and telemetry export.
use crate::{
    read::{NativeRead, ReadError, integer, invalid_data, optional_integer, optional_text, text},
    value::Rec,
};
use culprit_core::{
    Coverage, Pressure, Replica, ReplicaCondition, ReplicaRole, Result, Snapshot,
    telemetry::{AvailabilityReplicaSample, CollectionStop, GeoReplicationSample, ReplicationFact, canonical_time},
};
use std::collections::BTreeSet;
mod fields;
mod native;
mod selection;
use fields::{availability_replica, geo_link};
pub(crate) use selection::read_selected;

pub(crate) const MAX_REPLICAS: usize = 1024;
const LAYOUT: &str = r#"SELECT CAST(SERVERPROPERTY('IsHadrEnabled') AS int) AS hadr_enabled,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns
 WHERE object_id=OBJECT_ID(N'sys.dm_hadr_database_replica_states') AND name=N'secondary_lag_seconds')
 THEN 1 ELSE 0 END AS lag_column,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns
 WHERE object_id=OBJECT_ID(N'sys.dm_hadr_database_replica_states') AND name=N'is_primary_replica')
 THEN 1 ELSE 0 END AS primary_column,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.availability_groups') AND name=N'is_distributed') THEN 1 ELSE 0 END AS distributed_column,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.availability_groups') AND name=N'basic_features') THEN 1 ELSE 0 END AS basic_column,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.availability_groups') AND name=N'dtc_support') THEN 1 ELSE 0 END AS dtc_column,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.availability_groups') AND name=N'db_failover') THEN 1 ELSE 0 END AS database_failover_column,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.availability_groups') AND name=N'cluster_type') THEN 1 ELSE 0 END AS cluster_column,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.availability_groups') AND name=N'required_synchronized_secondaries_to_commit') THEN 1 ELSE 0 END AS required_secondaries_column,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.availability_groups') AND name=N'sequence_number') THEN 1 ELSE 0 END AS sequence_column,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.availability_replicas') AND name=N'seeding_mode') THEN 1 ELSE 0 END AS seeding_column,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.availability_replicas') AND name=N'read_write_routing_url') THEN 1 ELSE 0 END AS read_write_routing_column"#;
const GEO: &str = r#"SELECT link_guid AS link_id, partner_server, partner_database, role,
 role_desc AS native_role, secondary_allow_connections,
 secondary_allow_connections_desc AS native_secondary_allow_connections,
 replication_state AS state, replication_state_desc AS native_state,
 CASE WHEN role=0 THEN replication_lag_sec END AS lag_seconds,
 CASE WHEN role=0 THEN CONVERT(varchar(40),last_replication,127) END AS last_replication_at,
 CONVERT(varchar(40),last_commit,127) AS last_commit_at
 FROM sys.dm_geo_replication_link_status OPTION(MAXDOP 1)"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Selection {
    AvailabilityGroup,
    GeoLinks,
}

#[derive(Default)]
pub(crate) struct AvailabilityColumns {
    pub lag: bool,
    pub primary_role: bool,
    pub distributed: bool,
    pub basic: bool,
    pub dtc: bool,
    pub database_failover: bool,
    pub cluster: bool,
    pub required_secondaries: bool,
    pub sequence: bool,
    pub seeding: bool,
    pub read_write_routing: bool,
}

pub(crate) fn availability_sql(columns: AvailabilityColumns) -> String {
    let lag = if columns.lag { "drs.secondary_lag_seconds" } else { "CAST(NULL AS bigint)" };
    let primary = if columns.primary_role { "CAST(drs.is_primary_replica AS int)" } else { "CAST(NULL AS int)" };
    let distributed = optional_column(columns.distributed, "ag.is_distributed", "bit");
    let basic = optional_column(columns.basic, "ag.basic_features", "bit");
    let dtc = optional_column(columns.dtc, "ag.dtc_support", "bit");
    let database_failover = optional_column(columns.database_failover, "ag.db_failover", "bit");
    let cluster = optional_column(columns.cluster, "ag.cluster_type", "tinyint");
    let native_cluster = optional_column(columns.cluster, "ag.cluster_type_desc", "nvarchar(60)");
    let required_secondaries =
        optional_column(columns.required_secondaries, "ag.required_synchronized_secondaries_to_commit", "int");
    let sequence = optional_column(columns.sequence, "ag.sequence_number", "bigint");
    let seeding = optional_column(columns.seeding, "ar.seeding_mode", "tinyint");
    let native_seeding = optional_column(columns.seeding, "ar.seeding_mode_desc", "nvarchar(60)");
    let read_write_routing = optional_column(columns.read_write_routing, "ar.read_write_routing_url", "nvarchar(256)");
    format!(
        r#"SELECT drs.group_id, drs.group_database_id, drs.replica_id,
 drs.database_id, DB_NAME(drs.database_id) AS database_name,
 ar.replica_server_name AS native_name, CAST(drs.is_local AS int) AS is_local,
 {primary} AS is_primary,
 drs.synchronization_state AS state, drs.synchronization_state_desc AS native_state,
 ar.availability_mode AS mode, ar.availability_mode_desc AS native_mode,
 CAST(drs.is_suspended AS int) AS is_suspended, drs.suspend_reason, drs.synchronization_health,
 drs.is_commit_participant, drs.database_state, drs.database_state_desc AS native_database_state,
 drs.synchronization_health_desc AS native_synchronization_health, drs.suspend_reason_desc AS native_suspend_reason,
 drs.log_send_queue_size AS log_send_queue_kb, drs.redo_queue_size AS redo_queue_kb,
 drs.log_send_rate AS log_send_rate_kb_per_second, drs.redo_rate AS redo_rate_kb_per_second,
 {lag} AS secondary_lag_seconds, drs.filestream_send_rate AS filestream_send_rate_kb_per_second,
 drs.low_water_mark_for_ghosts,
 CONVERT(varchar(25),drs.recovery_lsn) AS recovery_lsn, CONVERT(varchar(25),drs.truncation_lsn) AS truncation_lsn,
 CONVERT(varchar(25),drs.last_sent_lsn) AS last_sent_lsn, CONVERT(varchar(25),drs.last_received_lsn) AS last_received_lsn,
 CONVERT(varchar(25),drs.last_hardened_lsn) AS last_hardened_lsn, CONVERT(varchar(25),drs.last_redone_lsn) AS last_redone_lsn,
 CONVERT(varchar(25),drs.end_of_log_lsn) AS end_of_log_lsn, CONVERT(varchar(25),drs.last_commit_lsn) AS last_commit_lsn,
 CONVERT(varchar(40),drs.last_sent_time,126) AS last_sent_without_offset,
 CONVERT(varchar(40),drs.last_received_time,126) AS last_received_without_offset,
 CONVERT(varchar(40),drs.last_hardened_time,126) AS last_hardened_without_offset,
 CONVERT(varchar(40),drs.last_redone_time,126) AS last_redone_without_offset,
 CONVERT(varchar(40),drs.last_commit_time,126) AS last_commit_without_offset,
 ag.group_id AS group_catalog_id, ags.group_id AS group_state_id,
 ag.name AS group_name, ags.primary_replica AS group_primary_replica,
 ags.primary_recovery_health AS group_primary_recovery_health,
 ags.primary_recovery_health_desc AS group_native_primary_recovery_health,
 ags.secondary_recovery_health AS group_secondary_recovery_health,
 ags.secondary_recovery_health_desc AS group_native_secondary_recovery_health,
 ags.synchronization_health AS group_synchronization_health,
 ags.synchronization_health_desc AS group_native_synchronization_health,
 ag.failure_condition_level AS group_failure_condition_level, ag.health_check_timeout AS group_health_check_timeout_ms,
 ag.automated_backup_preference AS group_automated_backup_preference,
 ag.automated_backup_preference_desc AS group_native_automated_backup_preference,
 {distributed} AS group_is_distributed, {basic} AS group_is_basic, {dtc} AS group_dtc_support,
 {database_failover} AS group_database_failover, {cluster} AS group_cluster_type,
 {native_cluster} AS group_native_cluster_type, {required_secondaries} AS group_required_secondaries,
 {sequence} AS group_configuration_sequence,
 ar.replica_id AS replica_catalog_id, ars.replica_id AS replica_state_id,
 ars.role AS replica_role, ars.role_desc AS replica_native_role,
 ars.operational_state AS replica_operational_state, ars.operational_state_desc AS replica_native_operational_state,
 ars.connected_state AS replica_connected_state, ars.connected_state_desc AS replica_native_connected_state,
 ars.recovery_health AS replica_recovery_health, ars.recovery_health_desc AS replica_native_recovery_health,
 ars.synchronization_health AS replica_synchronization_health,
 ars.synchronization_health_desc AS replica_native_synchronization_health,
 ars.last_connect_error_number AS replica_last_connect_error_number,
 CONVERT(varchar(40),ars.last_connect_error_timestamp,126) AS replica_last_connect_error_without_offset,
 ar.failover_mode AS replica_failover_mode, ar.failover_mode_desc AS replica_native_failover_mode,
 ar.session_timeout AS replica_session_timeout_seconds,
 ar.primary_role_allow_connections AS replica_primary_role_allow_connections,
 ar.primary_role_allow_connections_desc AS replica_native_primary_role_allow_connections,
 ar.secondary_role_allow_connections AS replica_secondary_role_allow_connections,
 ar.secondary_role_allow_connections_desc AS replica_native_secondary_role_allow_connections,
 {seeding} AS replica_seeding_mode, {native_seeding} AS replica_native_seeding_mode,
 ar.endpoint_url AS replica_endpoint_url, ar.read_only_routing_url AS replica_read_only_routing_url,
 {read_write_routing} AS replica_read_write_routing_url, ar.backup_priority AS replica_backup_priority,
 CONVERT(varchar(40),ar.create_date,126) AS replica_created_without_offset,
 CONVERT(varchar(40),ar.modify_date,126) AS replica_modified_without_offset
 FROM sys.dm_hadr_database_replica_states drs
 LEFT JOIN sys.availability_replicas ar ON ar.replica_id=drs.replica_id AND ar.group_id=drs.group_id
 LEFT JOIN sys.dm_hadr_availability_replica_states ars ON ars.replica_id=drs.replica_id AND ars.group_id=drs.group_id
 LEFT JOIN sys.availability_groups ag ON ag.group_id=drs.group_id
 LEFT JOIN sys.dm_hadr_availability_group_states ags ON ags.group_id=drs.group_id
 WHERE drs.database_id=DB_ID() OPTION(MAXDOP 1)"#
    )
}
fn optional_column(available: bool, expression: &'static str, native_type: &'static str) -> String {
    if available { expression.into() } else { format!("CAST(NULL AS {native_type})") }
}

async fn read<R: NativeRead>(
    reader: &mut R,
    edition: i64,
    selection: Selection,
    limit: usize,
) -> Result<selection::Part> {
    let limit = limit.min(MAX_REPLICAS);
    let (sql, decode): (String, fn(&Rec) -> Result<ReplicationFact>) = match selection {
        Selection::GeoLinks => (GEO.into(), geo_link),
        Selection::AvailabilityGroup => {
            let layout = match native::read(reader, LAYOUT, 1).await? {
                native::NativeRows::Available(rows) => rows,
                native::NativeRows::Unavailable(error) => return Ok(selection::Part::Unavailable { selection, error }),
            };
            let [layout] = layout.as_slice() else { return Err(invalid_data()) };
            // IsHadrEnabled is documented for boxed SQL Server, not Managed Instance. MI reads its
            // supported AG interface even when this property is NULL or zero.
            if matches!(edition, 2..=4) {
                match optional_integer(layout, "hadr_enabled")? {
                    Some(0) => return Ok(selection::Part::observed(selection, vec![])),
                    Some(1) => {}
                    _ => return Err(CollectionStop::SourceUnavailable.error()),
                }
            }
            let columns = AvailabilityColumns {
                lag: layout_flag(layout, "lag_column")?,
                primary_role: layout_flag(layout, "primary_column")?,
                distributed: layout_flag(layout, "distributed_column")?,
                basic: layout_flag(layout, "basic_column")?,
                dtc: layout_flag(layout, "dtc_column")?,
                database_failover: layout_flag(layout, "database_failover_column")?,
                cluster: layout_flag(layout, "cluster_column")?,
                required_secondaries: layout_flag(layout, "required_secondaries_column")?,
                sequence: layout_flag(layout, "sequence_column")?,
                seeding: layout_flag(layout, "seeding_column")?,
                read_write_routing: layout_flag(layout, "read_write_routing_column")?,
            };
            (availability_sql(columns), availability_replica)
        }
    };
    let rows = match native::read(reader, &sql, limit + 1).await? {
        native::NativeRows::Available(rows) => rows,
        native::NativeRows::Unavailable(error) => return Ok(selection::Part::Unavailable { selection, error }),
    };
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    let mut keys = BTreeSet::new();
    let facts = rows
        .iter()
        .map(|row| {
            let fact = decode(row)?;
            if !fact.is_valid() || !keys.insert(fact.key()) {
                return Err(invalid_data());
            }
            Ok(fact)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(selection::Part::observed(selection, facts))
}

fn layout_flag(row: &Rec, name: &str) -> Result<bool> {
    match integer(row, name)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid_data()),
    }
}

fn native_code(row: &Rec, name: &str) -> Result<Option<u8>> {
    optional_integer(row, name)?.map(|value| value.try_into().map_err(|_| invalid_data())).transpose()
}

fn counter(row: &Rec, name: &str) -> Result<Option<String>> {
    Ok(optional_integer(row, name)?.map(|value| value.to_string()))
}
pub(crate) fn application(fact: &ReplicationFact) -> Option<Replica> {
    match fact {
        ReplicationFact::Native(_) => None,
        ReplicationFact::AvailabilityGroup(row) => {
            let (local, primary) = (row.is_local?, row.is_primary?);
            if local && primary {
                return None;
            }
            let sync = match row.mode {
                Some(0) => Some(culprit_core::t!("provider.mssql.replication.asynchronous_commit")),
                Some(1) => Some(culprit_core::t!("provider.mssql.replication.synchronous_commit")),
                _ => row.native_mode.clone(),
            };
            Some(Replica {
                condition: if row.state == Some(0) { ReplicaCondition::Stopped } else { ReplicaCondition::Reported },
                state: row.native_state.clone(),
                sync,
                lag_seconds: number(&row.secondary_lag_seconds),
                lag_bytes: bytes(&row.redo_queue_kb),
                unsent_bytes: bytes(&row.log_send_queue_kb),
                ..Replica::new(
                    row.native_name.as_deref().unwrap_or(&row.replica_id),
                    if local { ReplicaRole::ThisServer } else { ReplicaRole::Replica },
                )
            })
        }
        ReplicationFact::GeoLink(row) => {
            let role = match row.role {
                Some(0) => ReplicaRole::Replica,
                Some(1) => ReplicaRole::ThisServer,
                _ => return None,
            };
            let name = match (&row.partner_server, &row.partner_database) {
                (Some(server), Some(database)) => format!("{server}/{database}"),
                _ => row.link_id.clone(),
            };
            Some(Replica {
                state: row.native_state.clone(),
                sync: Some(culprit_core::t!("provider.mssql.replication.asynchronous")),
                lag_seconds: number(&row.lag_seconds),
                ..Replica::new(name, role)
            })
        }
    }
}
fn number(value: &Option<String>) -> Option<f64> {
    value.as_ref()?.parse::<u64>().ok().map(|value| value as f64)
}
fn bytes(value: &Option<String>) -> Option<f64> {
    value.as_ref()?.parse::<u64>().ok()?.checked_mul(1024).map(|value| value as f64)
}
fn direction_known(fact: &ReplicationFact) -> bool {
    match fact {
        ReplicationFact::Native(_) => false,
        ReplicationFact::AvailabilityGroup(row) => row.is_local.is_some() && row.is_primary.is_some(),
        ReplicationFact::GeoLink(row) => matches!(row.role, Some(0 | 1)),
    }
}
fn record_application_result(coverage: &mut Coverage, result: Result<Vec<ReplicationFact>>) -> Vec<Replica> {
    let Some(rows) = coverage.read_rows(&crate::sources::REPLICATION, result) else { return vec![] };
    if rows.iter().any(|row| !direction_known(row)) {
        coverage.partial(
            &crate::sources::REPLICATION,
            culprit_core::message!("provider.mssql.replication.direction_unknown"),
        );
    }
    rows.iter().filter_map(application).collect()
}
fn record_local_role(settings: &mut Vec<culprit_core::Setting>, facts: &[ReplicationFact]) {
    let secondary = facts.iter().any(|fact| match fact {
        ReplicationFact::Native(_) => false,
        ReplicationFact::AvailabilityGroup(row) => row.is_local == Some(true) && row.is_primary == Some(false),
        ReplicationFact::GeoLink(row) => row.role == Some(1),
    });
    if secondary && !settings.iter().any(|value| value.name == culprit_core::REPLICA_SETTING) {
        settings.push(culprit_core::Setting {
            name: culprit_core::REPLICA_SETTING.into(),
            value: "readable-secondary".into(),
            default_value: None,
            scope: "database".into(),
        });
    }
}
pub(crate) async fn collect(
    pool: std::sync::Arc<crate::conn::Pool>,
    probe: &crate::probe::Probe,
    snapshot: &mut Snapshot,
) {
    let mut reader = crate::read::ReadSession::new(pool, culprit_core::telemetry::CollectionBudget::default());
    collect_from(&mut reader, probe.engine_edition, &probe.grant_hint(), snapshot).await;
}

async fn collect_from<R: NativeRead>(reader: &mut R, edition: i64, grant_hint: &str, snapshot: &mut Snapshot) {
    use culprit_core::telemetry::{ReplicationReadContext, ReplicationSnapshot, SourceShutdown};
    let from = std::time::SystemTime::now();
    let result = read_selected(reader, edition, MAX_REPLICAS).await;
    let shutdown = reader.close().await;
    let until = std::time::SystemTime::now();
    let mut coverage = Coverage::default();
    if shutdown == SourceShutdown::Unconfirmed {
        match &result {
            Err(error) => coverage.source_failed(&crate::sources::REPLICATION, error),
            Ok(read) => {
                for part in &read.parts {
                    if let selection::Part::Unavailable { selection, error } = part {
                        let source = culprit_core::Source {
                            name: selection.source(),
                            reads: crate::sources::REPLICATION.reads,
                            delivers: &[],
                        };
                        coverage.source_failed(&source, error);
                    }
                }
            }
        }
        coverage.source_failed(&crate::sources::REPLICATION, &CollectionStop::CancellationUnconfirmed.error());
        coverage.append_to(snapshot);
        return;
    }
    let completed = result.and_then(|read| {
        let context = ReplicationReadContext::collector(from, until)?;
        let (observations, sources) = read.into_scan_observations();
        let raw = ReplicationSnapshot { context, sources, observations };
        if !raw.is_valid() {
            return Err(invalid_data());
        }
        Ok(raw)
    });
    match completed {
        Ok(raw) => {
            for source in &raw.sources {
                let rows = if source.state == culprit_core::SourceState::Unavailable {
                    let error = culprit_core::Error::new(
                        source.error_code.unwrap_or(culprit_core::ErrorCode::QueryFailed),
                        culprit_core::t!("provider.mssql.replication.native_unavailable"),
                    );
                    Err(if error.code == culprit_core::ErrorCode::PermissionDenied {
                        error.with_hint(grant_hint)
                    } else {
                        error
                    })
                } else {
                    Ok(raw
                        .observations
                        .iter()
                        .filter(|row| row.source == source.source)
                        .map(|row| row.fact.clone())
                        .collect::<Vec<_>>())
                };
                if let Ok(facts) = &rows {
                    record_local_role(&mut snapshot.settings, facts);
                }
                snapshot
                    .pressure
                    .get_or_insert_with(Pressure::default)
                    .replicas
                    .extend(record_application_result(&mut coverage, rows));
                if !source.complete_scope && source.state != culprit_core::SourceState::Unavailable {
                    coverage.partial(
                        &crate::sources::REPLICATION,
                        culprit_core::message!("provider.mssql.replication.visibility_partial"),
                    );
                }
            }
            snapshot.pressure.get_or_insert_with(Pressure::default).replication = Some(Box::new(raw));
        }
        Err(error) => coverage.source_failed(&crate::sources::REPLICATION, &error),
    }
    coverage.append_to(snapshot);
}

#[cfg(test)]
pub(crate) mod tests;
