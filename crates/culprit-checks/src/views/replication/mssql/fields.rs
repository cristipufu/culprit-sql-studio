//! Native field projection only; all state eligibility lives in the parent decision helpers.
use super::*;

pub(super) fn database_fields(sample: &AvailabilityReplicaSample) -> Vec<culprit_core::ReplicationField> {
    let suspended = sample.is_suspended == Some(true);
    vec![
        native("is_local", t!("view.replication.mssql.field.local"), sample.is_local.map(u8::from)),
        native("is_primary", t!("view.replication.mssql.field.primary"), sample.is_primary.map(u8::from)),
        native("state_code", t!("view.replication.mssql.field.state_code"), sample.state),
        native("state", t!("view.replication.mssql.field.state"), sample.native_state.as_deref()),
        native("mode_code", t!("view.replication.mssql.field.mode_code"), sample.mode),
        native("mode", t!("view.replication.mssql.field.mode"), sample.native_mode.as_deref()),
        native(
            "is_commit_participant",
            t!("view.replication.mssql.field.commit_participant"),
            sample.is_commit_participant.map(u8::from),
        ),
        native("database_state_code", t!("view.replication.mssql.field.database_state_code"), sample.database_state),
        native(
            "database_state",
            t!("view.replication.mssql.field.database_state"),
            sample.native_database_state.as_deref(),
        ),
        native("is_suspended", t!("view.replication.mssql.field.suspended"), sample.is_suspended.map(u8::from)),
        native(
            "suspend_reason_code",
            t!("view.replication.mssql.field.suspend_reason_code"),
            if suspended { sample.suspend_reason } else { None },
        ),
        native(
            "suspend_reason",
            t!("view.replication.mssql.field.suspend_reason"),
            if suspended { sample.native_suspend_reason.as_deref() } else { None },
        ),
        native(
            "synchronization_health_code",
            t!("view.replication.mssql.field.health_code"),
            sample.synchronization_health,
        ),
        native(
            "synchronization_health",
            t!("view.replication.mssql.field.health"),
            sample.native_synchronization_health.as_deref(),
        ),
    ]
}

pub(super) fn movement_fields(sample: &AvailabilityReplicaSample) -> Vec<culprit_core::ReplicationField> {
    vec![
        number("log_send_queue_kb", t!("view.replication.mssql.field.send_queue"), sample.log_send_queue_kb.as_deref()),
        number("redo_queue_kb", t!("view.replication.mssql.field.redo_queue"), sample.redo_queue_kb.as_deref()),
        number(
            "log_send_rate_kb_per_second",
            t!("view.replication.mssql.field.send_rate"),
            sample.log_send_rate_kb_per_second.as_deref(),
        ),
        number(
            "redo_rate_kb_per_second",
            t!("view.replication.mssql.field.redo_rate"),
            sample.redo_rate_kb_per_second.as_deref(),
        ),
        number(
            "filestream_send_rate_kb_per_second",
            t!("view.replication.mssql.field.filestream_rate"),
            sample.filestream_send_rate_kb_per_second.as_deref(),
        ),
        number(
            "secondary_lag_seconds",
            t!("view.replication.mssql.field.lag"),
            sample.secondary_lag_seconds.as_deref(),
        ),
        native(
            "low_water_mark_for_ghosts",
            t!("view.replication.mssql.field.ghost_watermark"),
            sample.low_water_mark_for_ghosts.as_deref(),
        ),
    ]
}

pub(super) fn position_fields(sample: &AvailabilityReplicaSample) -> Vec<culprit_core::ReplicationField> {
    vec![
        native("recovery_lsn", t!("view.replication.mssql.field.recovery_position"), sample.recovery_lsn.as_deref()),
        native(
            "truncation_lsn",
            t!("view.replication.mssql.field.truncation_position"),
            sample.truncation_lsn.as_deref(),
        ),
        native("last_sent_lsn", t!("view.replication.mssql.field.sent_position"), sample.last_sent_lsn.as_deref()),
        native(
            "last_received_lsn",
            t!("view.replication.mssql.field.received_position"),
            sample.last_received_lsn.as_deref(),
        ),
        native(
            "last_hardened_lsn",
            t!("view.replication.mssql.field.hardened_position"),
            sample.last_hardened_lsn.as_deref(),
        ),
        native("last_redone_lsn", t!("view.replication.mssql.field.redone_lsn"), sample.last_redone_lsn.as_deref()),
        native("end_of_log_lsn", t!("view.replication.mssql.field.end_position"), sample.end_of_log_lsn.as_deref()),
        native("last_commit_lsn", t!("view.replication.mssql.field.commit_lsn"), sample.last_commit_lsn.as_deref()),
    ]
}

pub(super) fn clock_fields(sample: &AvailabilityReplicaSample) -> Vec<culprit_core::ReplicationField> {
    vec![
        native(
            "last_sent_without_offset",
            t!("view.replication.mssql.field.sent_time"),
            sample.last_sent_without_offset.as_deref(),
        ),
        native(
            "last_received_without_offset",
            t!("view.replication.mssql.field.received_time"),
            sample.last_received_without_offset.as_deref(),
        ),
        native(
            "last_hardened_without_offset",
            t!("view.replication.mssql.field.hardened_time"),
            sample.last_hardened_without_offset.as_deref(),
        ),
        native(
            "last_redone_without_offset",
            t!("view.replication.mssql.field.redone_time"),
            sample.last_redone_without_offset.as_deref(),
        ),
        native(
            "last_commit_without_offset",
            t!("view.replication.mssql.field.commit_time"),
            sample.last_commit_without_offset.as_deref(),
        ),
    ]
}

pub(super) fn group_fields(sample: Option<&AvailabilityGroupContext>) -> Vec<culprit_core::ReplicationField> {
    let mut fields = vec![
        native(
            "group.catalog_observed",
            t!("view.replication.mssql.field.catalog_observed"),
            sample.map(|sample| u8::from(sample.catalog_observed)),
        ),
        native(
            "group.state_observed",
            t!("view.replication.mssql.field.state_observed"),
            sample.map(|sample| u8::from(sample.state_observed)),
        ),
    ];
    if let Some(sample) = sample {
        fields.extend([
            native("group.name", t!("view.replication.mssql.field.group_name"), sample.name.as_deref()),
            native(
                "group.primary_replica",
                t!("view.replication.mssql.field.group_primary"),
                sample.primary_replica.as_deref(),
            ),
            native(
                "group.primary_recovery_health_code",
                t!("view.replication.mssql.field.primary_recovery_code"),
                sample.primary_recovery_health,
            ),
            native(
                "group.primary_recovery_health",
                t!("view.replication.mssql.field.primary_recovery"),
                sample.native_primary_recovery_health.as_deref(),
            ),
            native(
                "group.secondary_recovery_health_code",
                t!("view.replication.mssql.field.secondary_recovery_code"),
                sample.secondary_recovery_health,
            ),
            native(
                "group.secondary_recovery_health",
                t!("view.replication.mssql.field.secondary_recovery"),
                sample.native_secondary_recovery_health.as_deref(),
            ),
            native(
                "group.synchronization_health_code",
                t!("view.replication.mssql.field.health_code"),
                sample.synchronization_health,
            ),
            native(
                "group.synchronization_health",
                t!("view.replication.mssql.field.health"),
                sample.native_synchronization_health.as_deref(),
            ),
            native(
                "group.failure_condition_level",
                t!("view.replication.mssql.field.failure_level"),
                sample.failure_condition_level,
            ),
            number(
                "group.health_check_timeout_ms",
                t!("view.replication.mssql.field.health_timeout"),
                sample.health_check_timeout_ms.as_deref(),
            ),
            native(
                "group.automated_backup_preference_code",
                t!("view.replication.mssql.field.backup_preference_code"),
                sample.automated_backup_preference,
            ),
            native(
                "group.automated_backup_preference",
                t!("view.replication.mssql.field.backup_preference"),
                sample.native_automated_backup_preference.as_deref(),
            ),
            native(
                "group.is_distributed",
                t!("view.replication.mssql.field.distributed"),
                sample.is_distributed.map(u8::from),
            ),
            native("group.is_basic", t!("view.replication.mssql.field.basic"), sample.is_basic.map(u8::from)),
            native("group.dtc_support", t!("view.replication.mssql.field.dtc"), sample.dtc_support.map(u8::from)),
            native(
                "group.database_failover",
                t!("view.replication.mssql.field.database_failover"),
                sample.database_failover.map(u8::from),
            ),
            native(
                "group.cluster_type_code",
                t!("view.replication.mssql.field.cluster_type_code"),
                sample.cluster_type,
            ),
            native(
                "group.cluster_type",
                t!("view.replication.mssql.field.cluster_type"),
                sample.native_cluster_type.as_deref(),
            ),
            number(
                "group.required_synchronized_secondaries_to_commit",
                t!("view.replication.mssql.field.required_secondaries"),
                sample.required_synchronized_secondaries_to_commit.as_deref(),
            ),
            native(
                "group.configuration_sequence",
                t!("view.replication.mssql.field.configuration_sequence"),
                sample.configuration_sequence.as_deref(),
            ),
        ]);
    }
    fields
}

pub(super) fn replica_fields(sample: Option<&AvailabilityReplicaContext>) -> Vec<culprit_core::ReplicationField> {
    let mut fields = vec![
        native(
            "replica.catalog_observed",
            t!("view.replication.mssql.field.catalog_observed"),
            sample.map(|sample| u8::from(sample.catalog_observed)),
        ),
        native(
            "replica.state_observed",
            t!("view.replication.mssql.field.state_observed"),
            sample.map(|sample| u8::from(sample.state_observed)),
        ),
    ];
    if let Some(sample) = sample {
        fields.extend([
            native("replica.role_code", t!("view.replication.mssql.field.role_code"), sample.role),
            native("replica.role", t!("view.replication.mssql.field.role"), sample.native_role.as_deref()),
            native(
                "replica.operational_state_code",
                t!("view.replication.mssql.field.operational_code"),
                sample.operational_state,
            ),
            native(
                "replica.operational_state",
                t!("view.replication.mssql.field.operational"),
                sample.native_operational_state.as_deref(),
            ),
            native(
                "replica.connected_state_code",
                t!("view.replication.mssql.field.connected_code"),
                sample.connected_state,
            ),
            native(
                "replica.connected_state",
                t!("view.replication.mssql.field.connected"),
                sample.native_connected_state.as_deref(),
            ),
            native(
                "replica.recovery_health_code",
                t!("view.replication.mssql.field.recovery_code"),
                sample.recovery_health,
            ),
            native(
                "replica.recovery_health",
                t!("view.replication.mssql.field.recovery"),
                sample.native_recovery_health.as_deref(),
            ),
            native(
                "replica.synchronization_health_code",
                t!("view.replication.mssql.field.health_code"),
                sample.synchronization_health,
            ),
            native(
                "replica.synchronization_health",
                t!("view.replication.mssql.field.health"),
                sample.native_synchronization_health.as_deref(),
            ),
            native(
                "replica.last_connect_error_number",
                t!("view.replication.mssql.field.last_error"),
                sample.last_connect_error_number,
            ),
            native(
                "replica.last_connect_error_without_offset",
                t!("view.replication.mssql.field.last_error_time"),
                sample.last_connect_error_without_offset.as_deref(),
            ),
            native(
                "replica.failover_mode_code",
                t!("view.replication.mssql.field.failover_mode_code"),
                sample.failover_mode,
            ),
            native(
                "replica.failover_mode",
                t!("view.replication.mssql.field.failover_mode"),
                sample.native_failover_mode.as_deref(),
            ),
            number(
                "replica.session_timeout_seconds",
                t!("view.replication.mssql.field.session_timeout"),
                sample.session_timeout_seconds.as_deref(),
            ),
            native(
                "replica.primary_role_allow_connections_code",
                t!("view.replication.mssql.field.primary_connections_code"),
                sample.primary_role_allow_connections,
            ),
            native(
                "replica.primary_role_allow_connections",
                t!("view.replication.mssql.field.primary_connections"),
                sample.native_primary_role_allow_connections.as_deref(),
            ),
            native(
                "replica.secondary_role_allow_connections_code",
                t!("view.replication.mssql.field.secondary_connections_code"),
                sample.secondary_role_allow_connections,
            ),
            native(
                "replica.secondary_role_allow_connections",
                t!("view.replication.mssql.field.secondary_connections"),
                sample.native_secondary_role_allow_connections.as_deref(),
            ),
            native("replica.seeding_mode_code", t!("view.replication.mssql.field.seeding_code"), sample.seeding_mode),
            native(
                "replica.seeding_mode",
                t!("view.replication.mssql.field.seeding"),
                sample.native_seeding_mode.as_deref(),
            ),
            native("replica.endpoint_url", t!("view.replication.mssql.field.endpoint"), sample.endpoint_url.as_deref()),
            native(
                "replica.read_only_routing_url",
                t!("view.replication.mssql.field.read_only_route"),
                sample.read_only_routing_url.as_deref(),
            ),
            native(
                "replica.read_write_routing_url",
                t!("view.replication.mssql.field.read_write_route"),
                sample.read_write_routing_url.as_deref(),
            ),
            native(
                "replica.backup_priority",
                t!("view.replication.mssql.field.backup_priority"),
                sample.backup_priority,
            ),
            native(
                "replica.created_without_offset",
                t!("view.replication.mssql.field.created_time"),
                sample.created_without_offset.as_deref(),
            ),
            native(
                "replica.modified_without_offset",
                t!("view.replication.mssql.field.modified_time"),
                sample.modified_without_offset.as_deref(),
            ),
        ]);
    }
    fields
}
