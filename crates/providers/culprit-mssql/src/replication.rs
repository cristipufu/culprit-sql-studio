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

pub(crate) const MAX_REPLICAS: usize = 1024;
const LAYOUT: &str = r#"SELECT CAST(SERVERPROPERTY('IsHadrEnabled') AS int) AS hadr_enabled,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns
 WHERE object_id=OBJECT_ID(N'sys.dm_hadr_database_replica_states') AND name=N'secondary_lag_seconds')
 THEN 1 ELSE 0 END AS lag_column"#;
const GEO: &str = r#"SELECT link_guid AS link_id, partner_server, partner_database, role,
 replication_state AS state, replication_state_desc AS native_state,
 CASE WHEN role=0 THEN replication_lag_sec END AS lag_seconds,
 CASE WHEN role=0 THEN CONVERT(varchar(40),last_replication,127) END AS last_replication_at
 FROM sys.dm_geo_replication_link_status OPTION(MAXDOP 1)"#;

#[derive(Clone, Copy)]
pub(crate) enum Selection {
    AvailabilityGroup,
    GeoLinks,
}

pub(crate) fn availability_sql(lag_column: bool) -> String {
    let lag = if lag_column { "drs.secondary_lag_seconds" } else { "CAST(NULL AS bigint)" };
    format!(
        r#"SELECT drs.group_id, drs.group_database_id, drs.replica_id,
 ar.replica_server_name AS native_name, CAST(drs.is_local AS int) AS is_local,
 CAST(drs.is_primary_replica AS int) AS is_primary,
 drs.synchronization_state AS state, drs.synchronization_state_desc AS native_state,
 ar.availability_mode AS mode, ar.availability_mode_desc AS native_mode,
 CAST(drs.is_suspended AS int) AS is_suspended, drs.suspend_reason, drs.synchronization_health,
 drs.log_send_queue_size AS log_send_queue_kb, drs.redo_queue_size AS redo_queue_kb,
 drs.log_send_rate AS log_send_rate_kb_per_second, drs.redo_rate AS redo_rate_kb_per_second,
 {lag} AS secondary_lag_seconds
 FROM sys.dm_hadr_database_replica_states drs
 LEFT JOIN sys.availability_replicas ar ON ar.replica_id=drs.replica_id AND ar.group_id=drs.group_id
 WHERE drs.database_id=DB_ID() OPTION(MAXDOP 1)"#
    )
}

pub(crate) async fn read<R: NativeRead>(
    reader: &mut R,
    selection: Selection,
    limit: usize,
) -> Result<Vec<ReplicationFact>> {
    let limit = limit.min(MAX_REPLICAS);
    let (sql, decode): (String, fn(&Rec) -> Result<ReplicationFact>) = match selection {
        Selection::GeoLinks => (GEO.into(), geo_link),
        Selection::AvailabilityGroup => {
            let layout = reader.query(LAYOUT, vec![], 1).await.map_err(ReadError::into_error)?;
            let [layout] = layout.as_slice() else { return Err(invalid_data()) };
            match optional_integer(layout, "hadr_enabled")? {
                Some(0) => return Ok(vec![]),
                Some(1) => {}
                _ => return Err(CollectionStop::SourceUnavailable.error()),
            }
            let lag_column = match integer(layout, "lag_column")? {
                0 => false,
                1 => true,
                _ => return Err(invalid_data()),
            };
            (availability_sql(lag_column), availability_replica)
        }
    };
    let rows = reader.query(&sql, vec![], limit + 1).await.map_err(ReadError::into_error)?;
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    let mut keys = BTreeSet::new();
    rows.iter()
        .map(|row| {
            let fact = decode(row)?;
            if !fact.is_valid() || !keys.insert(fact.key()) {
                return Err(invalid_data());
            }
            Ok(fact)
        })
        .collect()
}

fn native_code(row: &Rec, name: &str) -> Result<Option<u8>> {
    optional_integer(row, name)?.map(|value| value.try_into().map_err(|_| invalid_data())).transpose()
}

fn counter(row: &Rec, name: &str) -> Result<Option<String>> {
    Ok(optional_integer(row, name)?.map(|value| value.to_string()))
}
fn availability_replica(row: &Rec) -> Result<ReplicationFact> {
    Ok(ReplicationFact::AvailabilityGroup(AvailabilityReplicaSample {
        group_id: text(row, "group_id")?,
        group_database_id: text(row, "group_database_id")?,
        replica_id: text(row, "replica_id")?,
        native_name: optional_text(row, "native_name")?,
        is_local: crate::read::optional_boolean(row, "is_local")?,
        is_primary: crate::read::optional_boolean(row, "is_primary")?,
        state: native_code(row, "state")?,
        native_state: optional_text(row, "native_state")?,
        mode: native_code(row, "mode")?,
        native_mode: optional_text(row, "native_mode")?,
        is_suspended: crate::read::optional_boolean(row, "is_suspended")?,
        suspend_reason: native_code(row, "suspend_reason")?,
        synchronization_health: native_code(row, "synchronization_health")?,
        log_send_queue_kb: counter(row, "log_send_queue_kb")?,
        redo_queue_kb: counter(row, "redo_queue_kb")?,
        log_send_rate_kb_per_second: counter(row, "log_send_rate_kb_per_second")?,
        redo_rate_kb_per_second: counter(row, "redo_rate_kb_per_second")?,
        secondary_lag_seconds: counter(row, "secondary_lag_seconds")?,
    }))
}
fn geo_link(row: &Rec) -> Result<ReplicationFact> {
    let role = native_code(row, "role")?;
    let primary = role == Some(0);
    let lag = counter(row, "lag_seconds")?;
    let last_replication =
        optional_text(row, "last_replication_at")?.map(|value| canonical_time(&value)).transpose()?;
    if !primary && (lag.is_some() || last_replication.is_some()) {
        return Err(invalid_data());
    }
    Ok(ReplicationFact::GeoLink(GeoReplicationSample {
        link_id: text(row, "link_id")?,
        partner_server: optional_text(row, "partner_server")?,
        partner_database: optional_text(row, "partner_database")?,
        role,
        state: native_code(row, "state")?,
        native_state: optional_text(row, "native_state")?,
        lag_seconds: lag,
        last_replication_at: last_replication,
    }))
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
    let mut coverage = Coverage::default();
    let selections: &[Selection] = match probe.platform() {
        Some(culprit_core::Platform::AzureSqlDatabase) => &[Selection::GeoLinks],
        Some(culprit_core::Platform::AzureSqlManagedInstance) => &[Selection::AvailabilityGroup, Selection::GeoLinks],
        _ => &[Selection::AvailabilityGroup],
    };
    for selection in selections {
        let rows = read(&mut reader, *selection, MAX_REPLICAS).await.map_err(|error| {
            if error.code == culprit_core::ErrorCode::PermissionDenied {
                error.with_hint(probe.grant_hint())
            } else {
                error
            }
        });
        if let Ok(facts) = &rows {
            record_local_role(&mut snapshot.settings, facts);
        }
        snapshot
            .pressure
            .get_or_insert_with(Pressure::default)
            .replicas
            .extend(record_application_result(&mut coverage, rows));
    }
    if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
        coverage.source_failed(&crate::sources::REPLICATION, &CollectionStop::CancellationUnconfirmed.error());
    }
    coverage.append_to(snapshot);
}

#[cfg(test)]
pub(crate) mod tests;
