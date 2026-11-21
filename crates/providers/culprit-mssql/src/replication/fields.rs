//! The fixed native projection retains each health scope and never converts log positions into byte counters.
use super::*;
use culprit_core::telemetry::{AvailabilityGroupContext, AvailabilityReplicaContext};

pub(super) fn availability_replica(row: &Rec) -> Result<ReplicationFact> {
    let group_id = text(row, "group_id")?;
    let replica_id = text(row, "replica_id")?;
    Ok(ReplicationFact::AvailabilityGroup(Box::new(AvailabilityReplicaSample {
        group: group_context(row, &group_id)?,
        replica: replica_context(row, &replica_id)?,
        group_id,
        group_database_id: text(row, "group_database_id")?,
        replica_id,
        database_id: optional_integer(row, "database_id")?
            .map(|id| id.try_into().map_err(|_| invalid_data()))
            .transpose()?,
        database_name: optional_text(row, "database_name")?,
        native_name: optional_text(row, "native_name")?,
        is_local: crate::read::optional_boolean(row, "is_local")?,
        progress: database_progress(row)?,
    })))
}

pub(super) fn database_progress(row: &Rec) -> Result<culprit_core::telemetry::MssqlReplicaDatabaseProgress> {
    Ok(culprit_core::telemetry::MssqlReplicaDatabaseProgress {
        is_primary: crate::read::optional_boolean(row, "is_primary")?,
        state: native_code(row, "state")?,
        native_state: optional_text(row, "native_state")?,
        mode: native_code(row, "mode")?,
        native_mode: optional_text(row, "native_mode")?,
        is_commit_participant: crate::read::optional_boolean(row, "is_commit_participant")?,
        database_state: native_code(row, "database_state")?,
        native_database_state: optional_text(row, "native_database_state")?,
        is_suspended: crate::read::optional_boolean(row, "is_suspended")?,
        suspend_reason: native_code(row, "suspend_reason")?,
        synchronization_health: native_code(row, "synchronization_health")?,
        native_synchronization_health: optional_text(row, "native_synchronization_health")?,
        native_suspend_reason: optional_text(row, "native_suspend_reason")?,
        log_send_queue_kb: counter(row, "log_send_queue_kb")?,
        redo_queue_kb: counter(row, "redo_queue_kb")?,
        log_send_rate_kb_per_second: counter(row, "log_send_rate_kb_per_second")?,
        redo_rate_kb_per_second: counter(row, "redo_rate_kb_per_second")?,
        secondary_lag_seconds: counter(row, "secondary_lag_seconds")?,
        filestream_send_rate_kb_per_second: counter(row, "filestream_send_rate_kb_per_second")?,
        low_water_mark_for_ghosts: counter(row, "low_water_mark_for_ghosts")?,
        recovery_lsn: optional_text(row, "recovery_lsn")?,
        truncation_lsn: optional_text(row, "truncation_lsn")?,
        last_sent_lsn: optional_text(row, "last_sent_lsn")?,
        last_received_lsn: optional_text(row, "last_received_lsn")?,
        last_hardened_lsn: optional_text(row, "last_hardened_lsn")?,
        last_redone_lsn: optional_text(row, "last_redone_lsn")?,
        end_of_log_lsn: optional_text(row, "end_of_log_lsn")?,
        last_commit_lsn: optional_text(row, "last_commit_lsn")?,
        last_sent_without_offset: local_time(row, "last_sent_without_offset")?,
        last_received_without_offset: local_time(row, "last_received_without_offset")?,
        last_hardened_without_offset: local_time(row, "last_hardened_without_offset")?,
        last_redone_without_offset: local_time(row, "last_redone_without_offset")?,
        last_commit_without_offset: local_time(row, "last_commit_without_offset")?,
    })
}

fn group_context(row: &Rec, group: &str) -> Result<Option<Box<AvailabilityGroupContext>>> {
    let catalog_observed = joined(row, "group_catalog_id", group)?;
    let state_observed = joined(row, "group_state_id", group)?;
    if !catalog_observed && !state_observed {
        require_absent_context(row, "group_", &["group_id", "group_database_id"])?;
        return Ok(None);
    }
    Ok(Some(Box::new(AvailabilityGroupContext {
        catalog_observed,
        state_observed,
        name: optional_text(row, "group_name")?,
        primary_replica: optional_text(row, "group_primary_replica")?,
        primary_recovery_health: native_code(row, "group_primary_recovery_health")?,
        native_primary_recovery_health: optional_text(row, "group_native_primary_recovery_health")?,
        secondary_recovery_health: native_code(row, "group_secondary_recovery_health")?,
        native_secondary_recovery_health: optional_text(row, "group_native_secondary_recovery_health")?,
        synchronization_health: native_code(row, "group_synchronization_health")?,
        native_synchronization_health: optional_text(row, "group_native_synchronization_health")?,
        failure_condition_level: native_code(row, "group_failure_condition_level")?,
        health_check_timeout_ms: counter(row, "group_health_check_timeout_ms")?,
        automated_backup_preference: native_code(row, "group_automated_backup_preference")?,
        native_automated_backup_preference: optional_text(row, "group_native_automated_backup_preference")?,
        is_distributed: crate::read::optional_boolean(row, "group_is_distributed")?,
        is_basic: crate::read::optional_boolean(row, "group_is_basic")?,
        dtc_support: crate::read::optional_boolean(row, "group_dtc_support")?,
        database_failover: crate::read::optional_boolean(row, "group_database_failover")?,
        cluster_type: native_code(row, "group_cluster_type")?,
        native_cluster_type: optional_text(row, "group_native_cluster_type")?,
        required_synchronized_secondaries_to_commit: counter(row, "group_required_secondaries")?,
        configuration_sequence: counter(row, "group_configuration_sequence")?,
    })))
}

fn replica_context(row: &Rec, replica: &str) -> Result<Option<Box<AvailabilityReplicaContext>>> {
    let catalog_observed = joined(row, "replica_catalog_id", replica)?;
    let state_observed = joined(row, "replica_state_id", replica)?;
    if !catalog_observed && !state_observed {
        require_absent_context(row, "replica_", &["replica_id"])?;
        return Ok(None);
    }
    Ok(Some(Box::new(AvailabilityReplicaContext {
        catalog_observed,
        state_observed,
        role: native_code(row, "replica_role")?,
        native_role: optional_text(row, "replica_native_role")?,
        operational_state: native_code(row, "replica_operational_state")?,
        native_operational_state: optional_text(row, "replica_native_operational_state")?,
        connected_state: native_code(row, "replica_connected_state")?,
        native_connected_state: optional_text(row, "replica_native_connected_state")?,
        recovery_health: native_code(row, "replica_recovery_health")?,
        native_recovery_health: optional_text(row, "replica_native_recovery_health")?,
        synchronization_health: native_code(row, "replica_synchronization_health")?,
        native_synchronization_health: optional_text(row, "replica_native_synchronization_health")?,
        last_connect_error_number: crate::read::optional_signed_integer(row, "replica_last_connect_error_number")?
            .map(|number| number.try_into().map_err(|_| invalid_data()))
            .transpose()?,
        last_connect_error_without_offset: local_time(row, "replica_last_connect_error_without_offset")?,
        failover_mode: native_code(row, "replica_failover_mode")?,
        native_failover_mode: optional_text(row, "replica_native_failover_mode")?,
        session_timeout_seconds: counter(row, "replica_session_timeout_seconds")?,
        primary_role_allow_connections: native_code(row, "replica_primary_role_allow_connections")?,
        native_primary_role_allow_connections: optional_text(row, "replica_native_primary_role_allow_connections")?,
        secondary_role_allow_connections: native_code(row, "replica_secondary_role_allow_connections")?,
        native_secondary_role_allow_connections: optional_text(row, "replica_native_secondary_role_allow_connections")?,
        seeding_mode: native_code(row, "replica_seeding_mode")?,
        native_seeding_mode: optional_text(row, "replica_native_seeding_mode")?,
        endpoint_url: optional_text(row, "replica_endpoint_url")?,
        read_only_routing_url: optional_text(row, "replica_read_only_routing_url")?,
        read_write_routing_url: optional_text(row, "replica_read_write_routing_url")?,
        backup_priority: native_code(row, "replica_backup_priority")?,
        created_without_offset: local_time(row, "replica_created_without_offset")?,
        modified_without_offset: local_time(row, "replica_modified_without_offset")?,
    })))
}

fn joined(row: &Rec, field: &str, expected: &str) -> Result<bool> {
    match optional_text(row, field)? {
        None => Ok(false),
        Some(value) if value.eq_ignore_ascii_case(expected) => Ok(true),
        _ => Err(invalid_data()),
    }
}

fn require_absent_context(row: &Rec, prefix: &str, except: &[&str]) -> Result<()> {
    if row.columns().any(|(name, value)| {
        name.starts_with(prefix) && !except.contains(&name) && !matches!(value, crate::value::Val::Null)
    }) {
        Err(invalid_data())
    } else {
        Ok(())
    }
}

pub(super) fn local_time(row: &Rec, field: &str) -> Result<Option<String>> {
    optional_text(row, field)?.map(|value| crate::sql_time::native_time(&value)).transpose()
}

pub(super) fn geo_link(row: &Rec) -> Result<ReplicationFact> {
    let role = native_code(row, "role")?;
    let lag = counter(row, "lag_seconds")?;
    let last_replication_at =
        optional_text(row, "last_replication_at")?.map(|value| canonical_time(&value)).transpose()?;
    if role != Some(0) && (lag.is_some() || last_replication_at.is_some()) {
        return Err(invalid_data());
    }
    Ok(ReplicationFact::GeoLink(Box::new(GeoReplicationSample {
        link_id: text(row, "link_id")?,
        partner_server: optional_text(row, "partner_server")?,
        partner_database: optional_text(row, "partner_database")?,
        role,
        native_role: optional_text(row, "native_role")?,
        state: native_code(row, "state")?,
        native_state: optional_text(row, "native_state")?,
        lag_seconds: lag,
        last_replication_at,
        secondary_allow_connections: native_code(row, "secondary_allow_connections")?,
        native_secondary_allow_connections: optional_text(row, "native_secondary_allow_connections")?,
        last_commit_at: optional_text(row, "last_commit_at")?.map(|value| canonical_time(&value)).transpose()?,
    })))
}
