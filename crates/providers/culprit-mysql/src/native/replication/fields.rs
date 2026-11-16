//! SHOW field names vary by native product/version. Only this allowlist crosses the native boundary.
//! https://dev.mysql.com/doc/refman/8.4/en/show-replica-status.html
//! https://mariadb.com/docs/server/reference/sql-statements/administrative-sql-statements/show/show-replica-status
use super::*;

const MAX_PROPERTY_BYTES: usize = 2_048;
const FILTER_FIELDS: &[&str] = &[
    "replicate_do_db",
    "replicate_ignore_db",
    "replicate_do_table",
    "replicate_ignore_table",
    "replicate_wild_do_table",
    "replicate_wild_ignore_table",
    "replicate_ignore_server_ids",
    "replicate_rewrite_db",
    "replicate_do_domain_ids",
    "replicate_ignore_domain_ids",
];

fn column<'a>(row: &Rec, names: &'a [&str]) -> Option<&'a str> {
    names.iter().copied().find(|name| row.has_column(name))
}

pub(super) fn properties(row: &Rec) -> Result<BTreeMap<String, String>> {
    let mut properties = BTreeMap::new();
    for (name, alternatives) in [
        ("channel", &["channel_name", "connection_name"][..]),
        ("source_host", &["source_host", "master_host"]),
        ("io_running", &["replica_io_running", "slave_io_running"]),
        ("sql_running", &["replica_sql_running", "slave_sql_running"]),
    ] {
        let field = column(row, alternatives).ok_or_else(invalid)?;
        if let Some(value) = row.native_text(field)? {
            insert(&mut properties, name, value)?;
        }
    }
    for (name, alternatives) in [
        ("io_state", &["replica_io_state", "slave_io_state"][..]),
        ("sql_running_state", &["replica_sql_running_state", "slave_sql_running_state"]),
        ("sql_state", &["slave_sql_state"]),
        ("source_uuid", &["source_uuid", "master_uuid"]),
        ("source_log_file", &["source_log_file", "master_log_file"]),
        ("relay_source_log_file", &["relay_source_log_file", "relay_master_log_file"]),
        ("relay_log_file", &["relay_log_file"]),
        ("until_condition", &["until_condition"]),
        ("until_log_file", &["until_log_file"]),
        ("using_gtid", &["using_gtid"]),
        ("parallel_mode", &["parallel_mode"]),
    ] {
        if let Some(field) = column(row, alternatives)
            && let Some(value) = row.native_text(field)?
        {
            insert(&mut properties, name, value)?;
        }
    }
    for (name, field, compact) in [
        ("io_error_at_without_offset", "last_io_error_timestamp", true),
        ("sql_error_at_without_offset", "last_sql_error_timestamp", true),
        ("source_last_event_at_without_offset", "master_last_event_time", false),
        ("applied_last_event_at_without_offset", "slave_last_event_time", false),
    ] {
        if row.has_column(field)
            && let Some(value) = row.native_text(field)?
            && !value.is_empty()
        {
            if !native_timestamp(&value, compact) {
                return Err(invalid());
            }
            // SHOW supplies no offset. Preserve its representation, including MySQL's two-digit year;
            // neither the collector UTC bounds nor a source/replica clock difference qualifies it as UTC.
            insert(&mut properties, name, value)?;
        }
    }
    let mut withheld = Vec::new();
    for &field in FILTER_FIELDS {
        if row.has_column(field)
            && let Some(value) = row.native_text(field)?
        {
            if value.len() <= MAX_PROPERTY_BYTES {
                properties.insert(field.into(), value);
            } else {
                // Presence is positive evidence; a truncated rule is not the configured native rule.
                withheld.push(field);
            }
        }
    }
    if !withheld.is_empty() {
        properties.insert("withheld_fields".into(), withheld.join(","));
    }
    Ok(properties)
}

fn insert(properties: &mut BTreeMap<String, String>, name: &str, value: String) -> Result<()> {
    if value.len() > MAX_PROPERTY_BYTES || value.chars().any(char::is_control) {
        return Err(invalid());
    }
    properties.insert(name.into(), value);
    Ok(())
}

fn native_timestamp(value: &str, compact: bool) -> bool {
    let separators: &[(usize, u8)] = if compact {
        &[(6, b' '), (9, b':'), (12, b':')]
    } else {
        &[(4, b'-'), (7, b'-'), (10, b' '), (13, b':'), (16, b':')]
    };
    let expected_length = if compact { 15 } else { 19 };
    value.len() == expected_length
        && value.bytes().enumerate().all(|(index, byte)| {
            separators
                .iter()
                .find(|(position, _)| index == *position)
                .map_or_else(|| byte.is_ascii_digit(), |(_, expected)| byte == *expected)
        })
}

pub(super) fn counters(row: &Rec) -> Result<Vec<NativeCounterSample>> {
    use CounterUnit::{Bytes, Count, Seconds};
    let mut counters = Vec::new();
    for (name, alternatives, unit) in [
        ("lag-seconds", &["seconds_behind_source", "seconds_behind_master"][..], Seconds),
        ("configured-delay", &["sql_delay"], Seconds),
        ("remaining-delay", &["sql_remaining_delay"], Seconds),
        ("relay-log-bytes", &["relay_log_space"], Bytes),
        ("received-source-log-position", &["read_source_log_pos", "read_master_log_pos"], Bytes),
        ("executed-source-log-position", &["exec_source_log_pos", "exec_master_log_pos"], Bytes),
        ("relay-log-position", &["relay_log_pos"], Bytes),
        ("until-log-position", &["until_log_pos"], Bytes),
        ("skip-counter", &["skip_counter"], Count),
        ("io-error-code", &["last_io_errno"], Count),
        ("sql-error-code", &["last_sql_errno"], Count),
        ("source-server-id", &["source_server_id", "master_server_id"], Count),
        ("source-port", &["source_port", "master_port"], Count),
        ("connect-retry-seconds", &["connect_retry"], Seconds),
        ("source-retry-count", &["source_retry_count", "master_retry_count"], Count),
        ("auto-position", &["auto_position"], Count),
        ("max-relay-log-bytes", &["max_relay_log_size"], Bytes),
    ] {
        append(row, &mut counters, name, alternatives, unit, CounterNature::Gauge)?;
    }
    // These MariaDB ALL-channel fields are recorded totals with source-specific reset lifetimes. In
    // particular received heartbeats need not reset on thread restart. Uptime proves none of their resets.
    for (name, native_name) in [
        ("ddl-groups", "slave_ddl_groups"),
        ("non-transactional-groups", "slave_non_transactional_groups"),
        ("transactional-groups", "slave_transactional_groups"),
        ("retried-transactions", "retried_transactions"),
        ("executed-log-entries", "executed_log_entries"),
        ("received-heartbeats", "slave_received_heartbeats"),
    ] {
        append(row, &mut counters, name, &[native_name], Count, CounterNature::Cumulative)?;
    }
    Ok(counters)
}

fn append(
    row: &Rec,
    counters: &mut Vec<NativeCounterSample>,
    name: &str,
    alternatives: &[&str],
    unit: CounterUnit,
    nature: CounterNature,
) -> Result<()> {
    if let Some(field) = column(row, alternatives)
        && let Some(value) = row.unsigned(field)?
    {
        if matches!(name, "io-error-code" | "sql-error-code") && value > u64::from(u32::MAX) {
            return Err(invalid());
        }
        let mut counter = NativeCounterSample::gauge(name, value, unit, MeasurementScope::Server, SOURCE, field);
        counter.nature = nature;
        counters.push(counter);
    }
    Ok(())
}
