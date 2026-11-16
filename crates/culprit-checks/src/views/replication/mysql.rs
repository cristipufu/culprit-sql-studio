//! Recorded SHOW channel facts. Receiver state, applier state and historical errors remain independent.
#[cfg(test)]
mod tests;

use super::support::{counter, entry, measurement, native, property};
use culprit_core::{MeasurementScope, ReplicationCondition, ReplicationEntry, ReplicationFieldGroup, t, telemetry::*};

const SOURCE: &str = "replication-channels";

pub(super) fn explain(row: &ReplicationObservation, sample: &NativeReplicationSample) -> Option<ReplicationEntry> {
    let conditions = conditions(row, sample)?;
    if !matches!(&row.fact, ReplicationFact::Native(original) if original.as_ref() == sample) {
        return None;
    }
    let channel = match property(sample, "channel") {
        Some("") => t!("view.replication.mysql.default_channel"),
        Some(channel) => channel.into(),
        None => t!("view.replication.mysql.unknown_channel"),
    };
    let mut report = entry(
        row,
        MeasurementScope::Server,
        t!("view.replication.mysql.title", channel = channel),
        t!("view.replication.mysql.explanation"),
    );
    report.conditions = conditions;
    report.fields = vec![
        native("source_host", t!("view.replication.mysql.field.source_host"), property(sample, "source_host")),
        native("io_running", t!("view.replication.mysql.field.io_running"), property(sample, "io_running")),
        native("sql_running", t!("view.replication.mysql.field.sql_running"), property(sample, "sql_running")),
        native("io_state", t!("view.replication.mysql.field.io_state"), property(sample, "io_state")),
        native(
            "sql_running_state",
            t!("view.replication.mysql.field.sql_running_state"),
            property(sample, "sql_running_state"),
        ),
        native("sql_state", t!("view.replication.mysql.field.sql_state"), property(sample, "sql_state")),
    ];
    report.groups = vec![
        positions(sample),
        delay(sample),
        errors(sample),
        configuration(sample),
        parallel(sample),
        filters(sample),
    ];
    report.investigation = t!("view.replication.mysql.investigate");
    report.verify = t!("view.replication.mysql.verify");
    Some(report)
}

pub(super) fn conditions(
    row: &ReplicationObservation,
    sample: &NativeReplicationSample,
) -> Option<Vec<ReplicationCondition>> {
    if row.source != SOURCE
        || sample.measurements.scope != MeasurementScope::Server
        || !matches!(&row.fact, ReplicationFact::Native(_))
    {
        return None;
    }
    use ReplicationCondition::{ApplyingStopped, Connecting, ReceiverStopped, ThreadsStopped};
    let io = property(sample, "io_running");
    let sql = property(sample, "sql_running");
    if io == Some("No") && sql == Some("No") {
        return Some(vec![ThreadsStopped]);
    }
    let mut conditions = vec![];
    match io {
        Some("No") => conditions.push(ReceiverStopped),
        Some("Connecting") => conditions.push(Connecting),
        _ => {}
    }
    if sql == Some("No") {
        conditions.push(ApplyingStopped);
    }
    Some(conditions)
}

fn positions(sample: &NativeReplicationSample) -> ReplicationFieldGroup {
    ReplicationFieldGroup {
        title: t!("view.replication.mysql.group.positions"),
        fields: vec![
            native(
                "source_log_file",
                t!("view.replication.mysql.field.source_log_file"),
                property(sample, "source_log_file"),
            ),
            counter(
                "received-source-log-position",
                t!("view.replication.mysql.field.received_position"),
                measurement(sample, "received-source-log-position"),
            ),
            native(
                "relay_source_log_file",
                t!("view.replication.mysql.field.applied_log_file"),
                property(sample, "relay_source_log_file"),
            ),
            counter(
                "executed-source-log-position",
                t!("view.replication.mysql.field.applied_position"),
                measurement(sample, "executed-source-log-position"),
            ),
            native(
                "relay_log_file",
                t!("view.replication.mysql.field.relay_log_file"),
                property(sample, "relay_log_file"),
            ),
            counter(
                "relay-log-position",
                t!("view.replication.mysql.field.relay_position"),
                measurement(sample, "relay-log-position"),
            ),
            counter(
                "relay-log-bytes",
                t!("view.replication.mysql.field.relay_bytes"),
                measurement(sample, "relay-log-bytes"),
            ),
            native(
                "source_last_event_at_without_offset",
                t!("view.replication.mysql.field.source_event_time"),
                property(sample, "source_last_event_at_without_offset"),
            ),
            native(
                "applied_last_event_at_without_offset",
                t!("view.replication.mysql.field.applied_event_time"),
                property(sample, "applied_last_event_at_without_offset"),
            ),
        ],
    }
}

fn delay(sample: &NativeReplicationSample) -> ReplicationFieldGroup {
    ReplicationFieldGroup {
        title: t!("view.replication.mysql.group.delay"),
        fields: vec![
            counter("lag-seconds", t!("view.replication.mysql.field.lag"), measurement(sample, "lag-seconds")),
            counter(
                "configured-delay",
                t!("view.replication.mysql.field.configured_delay"),
                measurement(sample, "configured-delay"),
            ),
            counter(
                "remaining-delay",
                t!("view.replication.mysql.field.remaining_delay"),
                measurement(sample, "remaining-delay"),
            ),
            counter(
                "skip-counter",
                t!("view.replication.mysql.field.skip_counter"),
                measurement(sample, "skip-counter"),
            ),
            native(
                "until_condition",
                t!("view.replication.mysql.field.until_condition"),
                property(sample, "until_condition"),
            ),
            native(
                "until_log_file",
                t!("view.replication.mysql.field.until_log_file"),
                property(sample, "until_log_file"),
            ),
            counter(
                "until-log-position",
                t!("view.replication.mysql.field.until_position"),
                measurement(sample, "until-log-position"),
            ),
        ],
    }
}

fn errors(sample: &NativeReplicationSample) -> ReplicationFieldGroup {
    ReplicationFieldGroup {
        title: t!("view.replication.mysql.group.errors"),
        fields: vec![
            native("io-error-code", t!("view.replication.mysql.field.io_error"), integer(sample, "io-error-code")),
            native(
                "io_error_at_without_offset",
                t!("view.replication.mysql.field.io_error_time"),
                property(sample, "io_error_at_without_offset"),
            ),
            native("sql-error-code", t!("view.replication.mysql.field.sql_error"), integer(sample, "sql-error-code")),
            native(
                "sql_error_at_without_offset",
                t!("view.replication.mysql.field.sql_error_time"),
                property(sample, "sql_error_at_without_offset"),
            ),
        ],
    }
}

fn configuration(sample: &NativeReplicationSample) -> ReplicationFieldGroup {
    ReplicationFieldGroup {
        title: t!("view.replication.mysql.group.configuration"),
        fields: vec![
            native("source_uuid", t!("view.replication.mysql.field.source_uuid"), property(sample, "source_uuid")),
            native(
                "source-server-id",
                t!("view.replication.mysql.field.source_id"),
                integer(sample, "source-server-id"),
            ),
            native("source-port", t!("view.replication.mysql.field.source_port"), integer(sample, "source-port")),
            counter(
                "connect-retry-seconds",
                t!("view.replication.mysql.field.connect_retry"),
                measurement(sample, "connect-retry-seconds"),
            ),
            counter(
                "source-retry-count",
                t!("view.replication.mysql.field.retry_count"),
                measurement(sample, "source-retry-count"),
            ),
            native("auto-position", t!("view.replication.mysql.field.auto_position"), integer(sample, "auto-position")),
            native("using_gtid", t!("view.replication.mysql.field.using_gtid"), property(sample, "using_gtid")),
            native(
                "parallel_mode",
                t!("view.replication.mysql.field.parallel_mode"),
                property(sample, "parallel_mode"),
            ),
            counter(
                "max-relay-log-bytes",
                t!("view.replication.mysql.field.max_relay_bytes"),
                measurement(sample, "max-relay-log-bytes"),
            ),
        ],
    }
}

fn parallel(sample: &NativeReplicationSample) -> ReplicationFieldGroup {
    ReplicationFieldGroup {
        title: t!("view.replication.mysql.group.parallel"),
        fields: vec![
            counter("ddl-groups", t!("view.replication.mysql.field.ddl_groups"), measurement(sample, "ddl-groups")),
            counter(
                "non-transactional-groups",
                t!("view.replication.mysql.field.non_transactional_groups"),
                measurement(sample, "non-transactional-groups"),
            ),
            counter(
                "transactional-groups",
                t!("view.replication.mysql.field.transactional_groups"),
                measurement(sample, "transactional-groups"),
            ),
            counter(
                "retried-transactions",
                t!("view.replication.mysql.field.retried_transactions"),
                measurement(sample, "retried-transactions"),
            ),
            counter(
                "executed-log-entries",
                t!("view.replication.mysql.field.executed_entries"),
                measurement(sample, "executed-log-entries"),
            ),
            counter(
                "received-heartbeats",
                t!("view.replication.mysql.field.heartbeats"),
                measurement(sample, "received-heartbeats"),
            ),
        ],
    }
}

fn filters(sample: &NativeReplicationSample) -> ReplicationFieldGroup {
    let mut fields = vec![
        native("replicate_do_db", t!("view.replication.mysql.field.do_db"), property(sample, "replicate_do_db")),
        native(
            "replicate_ignore_db",
            t!("view.replication.mysql.field.ignore_db"),
            property(sample, "replicate_ignore_db"),
        ),
        native(
            "replicate_do_table",
            t!("view.replication.mysql.field.do_table"),
            property(sample, "replicate_do_table"),
        ),
        native(
            "replicate_ignore_table",
            t!("view.replication.mysql.field.ignore_table"),
            property(sample, "replicate_ignore_table"),
        ),
        native(
            "replicate_wild_do_table",
            t!("view.replication.mysql.field.wild_do_table"),
            property(sample, "replicate_wild_do_table"),
        ),
        native(
            "replicate_wild_ignore_table",
            t!("view.replication.mysql.field.wild_ignore_table"),
            property(sample, "replicate_wild_ignore_table"),
        ),
        native(
            "replicate_ignore_server_ids",
            t!("view.replication.mysql.field.ignore_servers"),
            property(sample, "replicate_ignore_server_ids"),
        ),
        native(
            "replicate_rewrite_db",
            t!("view.replication.mysql.field.rewrite_db"),
            property(sample, "replicate_rewrite_db"),
        ),
        native(
            "replicate_do_domain_ids",
            t!("view.replication.mysql.field.do_domains"),
            property(sample, "replicate_do_domain_ids"),
        ),
        native(
            "replicate_ignore_domain_ids",
            t!("view.replication.mysql.field.ignore_domains"),
            property(sample, "replicate_ignore_domain_ids"),
        ),
    ];
    if let Some(withheld) = property(sample, "withheld_fields") {
        fields.push(native("withheld_fields", t!("view.replication.mysql.field.withheld"), Some(withheld)));
    }
    ReplicationFieldGroup { title: t!("view.replication.mysql.group.filters"), fields }
}

fn integer(sample: &NativeReplicationSample, key: &str) -> Option<u64> {
    measurement(sample, key)?.value.integer()
}
