//! SQL Server's native health scopes and clocks remain separate, without a query-owner or drain-time inference.
use super::support::{entry, native, number};
mod fields;
use culprit_core::{MeasurementScope, ReplicationCondition, ReplicationEntry, ReplicationFieldGroup, t, telemetry::*};
use fields::{clock_fields, database_fields, group_fields, movement_fields, position_fields, replica_fields};

pub(super) fn availability_conditions(sample: &AvailabilityReplicaSample) -> Vec<ReplicationCondition> {
    let mut conditions = database_conditions(&sample.progress);
    if sample.replica.as_ref().and_then(|replica| replica.connected_state) == Some(0) {
        conditions.push(ReplicationCondition::Disconnected);
    }
    conditions
}

pub(super) fn database_conditions(progress: &MssqlReplicaDatabaseProgress) -> Vec<ReplicationCondition> {
    let mut conditions = Vec::new();
    if progress.is_suspended == Some(true) {
        conditions.push(ReplicationCondition::MovementSuspended)
    }
    if progress.state == Some(0) {
        conditions.push(ReplicationCondition::NotSynchronizing)
    }
    conditions
}

pub(super) fn geo_conditions(sample: &GeoReplicationSample) -> Vec<ReplicationCondition> {
    if sample.state == Some(4) { vec![ReplicationCondition::MovementSuspended] } else { vec![] }
}

pub(super) fn availability(row: &ReplicationObservation, sample: &AvailabilityReplicaSample) -> ReplicationEntry {
    let conditions = availability_conditions(sample);
    let mut result = entry(
        row,
        MeasurementScope::Database,
        t!("view.replication.mssql.availability.title"),
        availability_explanation(sample, &conditions),
    );
    result.conditions = conditions;
    result.fields = vec![
        native("database_id", t!("view.replication.mssql.field.database_id"), sample.database_id),
        native("database_name", t!("view.replication.mssql.field.database_name"), sample.database_name.as_deref()),
        native("group_id", t!("view.replication.mssql.field.group_id"), Some(sample.group_id.as_str())),
        native(
            "group_database_id",
            t!("view.replication.mssql.field.group_database_id"),
            Some(sample.group_database_id.as_str()),
        ),
        native("replica_id", t!("view.replication.mssql.field.replica_id"), Some(sample.replica_id.as_str())),
        native("replica_name", t!("view.replication.mssql.field.replica_name"), sample.native_name.as_deref()),
    ];
    result.groups = vec![
        ReplicationFieldGroup {
            title: t!("view.replication.mssql.group.database"),
            fields: database_fields(&sample.progress, sample.is_local),
        },
        ReplicationFieldGroup {
            title: t!("view.replication.mssql.group.movement"),
            fields: movement_fields(&sample.progress),
        },
        ReplicationFieldGroup {
            title: t!("view.replication.mssql.group.positions"),
            fields: position_fields(&sample.progress),
        },
        ReplicationFieldGroup {
            title: t!("view.replication.mssql.group.clocks"),
            fields: clock_fields(&sample.progress),
        },
        ReplicationFieldGroup {
            title: t!("view.replication.mssql.group.group"),
            fields: group_fields(sample.group.as_deref()),
        },
        ReplicationFieldGroup {
            title: t!("view.replication.mssql.group.replica"),
            fields: replica_fields(sample.replica.as_deref()),
        },
    ];
    result.investigation = t!("view.replication.mssql.availability.investigate");
    result.verify = t!("view.replication.mssql.availability.verify");
    result
}

fn availability_explanation(sample: &AvailabilityReplicaSample, conditions: &[ReplicationCondition]) -> String {
    let mut words = vec![t!("view.replication.mssql.availability.explanation")];
    if conditions.contains(&ReplicationCondition::MovementSuspended) {
        words.push(t!("view.replication.mssql.availability.suspended"));
    } else if conditions.contains(&ReplicationCondition::NotSynchronizing) {
        words.push(t!("view.replication.mssql.availability.not_synchronizing"));
    }
    if conditions.contains(&ReplicationCondition::Disconnected) {
        words.push(t!("view.replication.mssql.availability.disconnected"));
    }
    if sample.replica.as_ref().and_then(|replica| replica.last_connect_error_number).is_some_and(|number| number != 0) {
        words.push(t!("view.replication.mssql.availability.historical_error"));
    }
    words.join(" ")
}

pub(super) fn geo(row: &ReplicationObservation, sample: &GeoReplicationSample) -> ReplicationEntry {
    let explanation = match sample.role {
        Some(0) => t!("view.replication.mssql.geo.primary"),
        Some(1) => t!("view.replication.mssql.geo.secondary"),
        _ => t!("view.replication.mssql.geo.unknown"),
    };
    let mut result = entry(row, MeasurementScope::Database, t!("view.replication.mssql.geo.title"), explanation);
    result.conditions = geo_conditions(sample);
    if result.conditions.contains(&ReplicationCondition::MovementSuspended) {
        result.explanation.push(' ');
        result.explanation.push_str(&t!("view.replication.mssql.geo.suspended"));
    } else if sample.state == Some(1) {
        result.explanation.push(' ');
        result.explanation.push_str(&t!("view.replication.mssql.geo.seeding"));
    }
    let primary = sample.role == Some(0);
    result.fields = vec![
        native("link_id", t!("view.replication.mssql.field.link_id"), Some(sample.link_id.as_str())),
        native("partner_server", t!("view.replication.mssql.field.partner_server"), sample.partner_server.as_deref()),
        native(
            "partner_database",
            t!("view.replication.mssql.field.partner_database"),
            sample.partner_database.as_deref(),
        ),
        native("role_code", t!("view.replication.mssql.field.role_code"), sample.role),
        native("role", t!("view.replication.mssql.field.role"), sample.native_role.as_deref()),
        native("state_code", t!("view.replication.mssql.field.geo_state_code"), sample.state),
        native("state", t!("view.replication.mssql.field.geo_state"), sample.native_state.as_deref()),
        number(
            "lag_seconds",
            t!("view.replication.mssql.field.geo_lag"),
            if primary { sample.lag_seconds.as_deref() } else { None },
        ),
        native(
            "last_replication_at",
            t!("view.replication.mssql.field.last_replication"),
            if primary { sample.last_replication_at.as_deref() } else { None },
        ),
        native(
            "last_commit_at",
            t!("view.replication.mssql.field.last_local_commit"),
            sample.last_commit_at.as_deref(),
        ),
        native(
            "secondary_allow_connections_code",
            t!("view.replication.mssql.field.secondary_connections_code"),
            sample.secondary_allow_connections,
        ),
        native(
            "secondary_allow_connections",
            t!("view.replication.mssql.field.secondary_connections"),
            sample.native_secondary_allow_connections.as_deref(),
        ),
    ];
    result.investigation = t!("view.replication.mssql.geo.investigate");
    result.verify = t!("view.replication.mssql.geo.verify");
    result
}

pub(super) fn hyperscale(row: &ReplicationObservation, sample: &MssqlHyperscaleReplica) -> ReplicationEntry {
    let mut result = entry(
        row,
        MeasurementScope::Database,
        t!("view.replication.mssql.hyperscale.title"),
        t!("view.replication.mssql.hyperscale.explanation"),
    );
    result.fields = vec![
        native("replica_id", t!("view.replication.mssql.field.replica_id"), Some(sample.replica_id.as_str())),
        native(
            "replica_server_name",
            t!("view.replication.mssql.hyperscale.logical_server"),
            sample.replica_server_name.as_deref(),
        ),
        native(
            "replica_database_name",
            t!("view.replication.mssql.field.database_name"),
            sample.replica_database_name.as_deref(),
        ),
        native("is_local", t!("view.replication.mssql.field.local"), sample.is_local),
        native("role_code", t!("view.replication.mssql.field.role_code"), sample.replica_role),
        native("role", t!("view.replication.mssql.field.role"), sample.native_role.as_deref()),
        native(
            "local_database_id",
            t!("view.replication.mssql.hyperscale.local_database_id"),
            sample.local_database_id,
        ),
        native(
            "parent_replica_id",
            t!("view.replication.mssql.hyperscale.parent_replica_id"),
            sample.parent_replica_id.as_deref(),
        ),
        native(
            "selected_database_id",
            t!("view.replication.mssql.hyperscale.selected_database_id"),
            Some(sample.selected_database_id),
        ),
        native(
            "observer_replica_id",
            t!("view.replication.mssql.hyperscale.observer_replica_id"),
            Some(sample.observer_replica_id.as_str()),
        ),
        native("observed_at", t!("view.replication.mssql.hyperscale.observed_at"), Some(sample.observed_at.as_str())),
    ];
    result.investigation = t!("view.replication.mssql.hyperscale.investigate");
    result.verify = t!("view.replication.mssql.hyperscale.verify");
    result
}

pub(super) fn azure_local(row: &ReplicationObservation, point: &MssqlAzureReplicaState) -> ReplicationEntry {
    let mut result = entry(
        row,
        MeasurementScope::Database,
        t!("view.replication.mssql.azure_local.title"),
        t!("view.replication.mssql.azure_local.explanation"),
    );
    result.conditions = database_conditions(&point.progress);
    result.fields = vec![
        native("database_id", t!("view.replication.mssql.field.database_id"), Some(point.database_id)),
        native("replica_id", t!("view.replication.mssql.field.replica_id"), Some(point.replica_id.as_str())),
        native("group_id", t!("view.replication.mssql.field.group_id"), point.group_id.as_deref()),
        native(
            "group_database_id",
            t!("view.replication.mssql.field.group_database_id"),
            point.group_database_id.as_deref(),
        ),
        native("observed_at", t!("view.replication.mssql.hyperscale.observed_at"), Some(point.observed_at.as_str())),
    ];
    result.groups = vec![
        ReplicationFieldGroup {
            title: t!("view.replication.mssql.group.database"),
            fields: database_fields(&point.progress, Some(point.is_local)),
        },
        ReplicationFieldGroup {
            title: t!("view.replication.mssql.group.movement"),
            fields: movement_fields(&point.progress),
        },
        ReplicationFieldGroup {
            title: t!("view.replication.mssql.group.positions"),
            fields: position_fields(&point.progress),
        },
        ReplicationFieldGroup {
            title: t!("view.replication.mssql.group.clocks"),
            fields: clock_fields(&point.progress),
        },
    ];
    result.investigation = t!("view.replication.mssql.azure_local.investigate");
    result.verify = t!("view.replication.mssql.azure_local.verify");
    result
}

#[cfg(test)]
mod tests;
