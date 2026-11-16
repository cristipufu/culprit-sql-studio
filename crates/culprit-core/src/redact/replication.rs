//! Sharing retains native measurements without publishing replication endpoints or current lookup identities.
use super::{opaque, redact_native_identity, redact_read_context};
use crate::{ResetMarker, Result, telemetry::*};
use std::collections::BTreeMap;

pub(super) fn redact(mut read: Box<ReplicationSnapshot>) -> Result<Box<ReplicationSnapshot>> {
    if !read.is_valid_read(false) {
        return Err(CollectionStop::InvalidPage.error());
    }
    if let ObservationReadContext::Native { context } = &mut read.context {
        redact_read_context(context);
    }
    for row in &mut read.observations {
        match &mut row.fact {
            ReplicationFact::AvailabilityGroup(sample) => availability(sample),
            ReplicationFact::GeoLink(sample) => {
                pseudonymize(&mut sample.partner_server);
                pseudonymize(&mut sample.partner_database);
            }
            ReplicationFact::Native(sample) => native(sample)?,
        }
    }
    read.is_valid_read(false).then_some(read).ok_or_else(|| CollectionStop::InvalidPage.error())
}

fn availability(sample: &mut AvailabilityReplicaSample) {
    // Native GUIDs are already opaque identifiers with a validated representation. Keep their equality/key.
    for name in [&mut sample.database_name, &mut sample.native_name] {
        pseudonymize(name);
    }
    if let Some(group) = &mut sample.group {
        pseudonymize(&mut group.name);
        pseudonymize(&mut group.primary_replica);
    }
    if let Some(replica) = &mut sample.replica {
        for url in [&mut replica.endpoint_url, &mut replica.read_only_routing_url, &mut replica.read_write_routing_url]
        {
            pseudonymize(url);
        }
    }
}

fn native(sample: &mut NativeReplicationSample) -> Result<()> {
    redact_native_identity(&mut sample.measurements.identity);
    properties(&mut sample.measurements.properties);
    for counter in &mut sample.measurements.counters {
        // Names identify native counter schemas; instances and reset generations can identify physical owners.
        for name in [&mut counter.key, &mut counter.native_object, &mut counter.native_name] {
            if !schema_name(name) {
                *name = opaque(name);
            }
        }
        pseudonymize(&mut counter.native_instance);
        if let Some(native_type) = &mut counter.native_type
            && !native_type.bytes().all(|byte| byte.is_ascii_digit())
        {
            *native_type = opaque(native_type);
        }
        if let ResetMarker::Generation(generation) = &mut counter.reset {
            *generation = opaque(generation);
        }
    }
    if let Some(session) = &mut sample.session {
        session.id = opaque(&session.id);
        session.generation = opaque(&session.generation);
    }
    pseudonymize(&mut sample.unresolved_session_id);
    pseudonymize(&mut sample.slot_name);
    sample.key = sample.measurements.key()?;
    Ok(())
}

fn pseudonymize(value: &mut Option<String>) {
    if let Some(value) = value {
        *value = opaque(value);
    }
}

fn schema_name(value: &str) -> bool {
    value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn properties(values: &mut BTreeMap<String, String>) {
    *values = std::mem::take(values)
        .into_iter()
        .map(|(name, value)| {
            let public = public_property(&name, &value);
            let known = public.is_some() || private_property(&name);
            let redacted = if value.is_empty() || public == Some(true) { value } else { opaque(&value) };
            (if known { name } else { opaque(&name) }, redacted)
        })
        .collect();
}

fn private_property(name: &str) -> bool {
    matches!(
        name,
        "name"
            | "slot_name"
            | "active_pid"
            | "database"
            | "database_oid"
            | "read_database_oid"
            | "plugin"
            | "channel"
            | "source_host"
            | "source_uuid"
            | "source_log_file"
            | "relay_source_log_file"
            | "relay_log_file"
            | "until_log_file"
            | "replicate_do_db"
            | "replicate_ignore_db"
            | "replicate_do_table"
            | "replicate_ignore_table"
            | "replicate_wild_do_table"
            | "replicate_wild_ignore_table"
            | "replicate_ignore_server_ids"
            | "replicate_rewrite_db"
            | "replicate_do_domain_ids"
            | "replicate_ignore_domain_ids"
            | "io_state"
            | "sql_running_state"
            | "sql_state"
    )
}

fn public_property(name: &str, value: &str) -> Option<bool> {
    Some(match name {
        "kind" => matches!(value, "sender" | "standby" | "slot"),
        "slot_type" => matches!(value, "logical" | "physical"),
        "state" => matches!(value, "startup" | "catchup" | "streaming" | "backup" | "stopping"),
        "sync_state" => matches!(value, "async" | "potential" | "sync" | "quorum"),
        "wal_status" => matches!(value, "reserved" | "extended" | "unreserved" | "lost"),
        "invalidation_reason" => {
            matches!(value, "wal_removed" | "rows_removed" | "wal_level_insufficient" | "idle_timeout")
        }
        "receiver" => matches!(value, "startup" | "streaming" | "waiting" | "restarting" | "stopping" | "stopped"),
        "replay_pause_state" => matches!(value, "not paused" | "pause requested" | "paused"),
        "io_running" | "sql_running" => matches!(value, "Yes" | "No" | "Connecting"),
        "until_condition" => matches!(
            value,
            "None" | "Source" | "Master" | "Relay" | "SQL_BEFORE_GTIDS" | "SQL_AFTER_GTIDS" | "SQL_AFTER_MTS_GAPS"
        ),
        "using_gtid" => matches!(value, "No" | "Current_Pos" | "Slave_Pos"),
        "parallel_mode" => matches!(value, "none" | "minimal" | "conservative" | "optimistic" | "aggressive"),
        "active"
        | "logical"
        | "failover"
        | "synced"
        | "temporary"
        | "two_phase"
        | "conflicting"
        | "streaming_configured"
        | "replay_paused"
        | "replay_pause_requested"
        | "read_standby"
        | "full_visibility" => {
            matches!(value, "true" | "false")
        }
        "xmin" | "catalog_xmin" | "backend_xmin" | "sync_priority" | "receive_start_tli" | "received_tli"
        | "server_version_num" => !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
        "restart_lsn"
        | "confirmed_flush_lsn"
        | "sent_lsn"
        | "write_lsn"
        | "flush_lsn"
        | "replay_lsn"
        | "receive_lsn"
        | "current_lsn"
        | "two_phase_at"
        | "receive_start_lsn"
        | "receiver_received_lsn"
        | "written_lsn"
        | "flushed_lsn"
        | "latest_end_lsn" => postgres_position(value),
        "backend_start"
        | "reply_time"
        | "inactive_since"
        | "last_replay_commit_at"
        | "last_msg_send_time"
        | "last_msg_receipt_time"
        | "latest_end_time"
        | "observed_at" => chrono::DateTime::parse_from_rfc3339(value).is_ok(),
        "io_error_at_without_offset"
        | "sql_error_at_without_offset"
        | "source_last_event_at_without_offset"
        | "applied_last_event_at_without_offset" => local_clock(value),
        "write_lag" | "flush_lag" | "replay_lag" => interval(value),
        "behind_native"
        | "kept_native"
        | "safe_wal_size_native"
        | "lag_s_native"
        | "write_lag_s_native"
        | "flush_lag_s_native"
        | "inactive_s_native"
        | "since_replay_native"
        | "delay_s_native" => value.parse::<f64>().is_ok_and(f64::is_finite),
        "column_availability" => visibility(value, COLUMN_AVAILABILITY),
        "replay_pause_access" => visibility(value, &["paused", "requested", "state"]),
        "withheld_fields" => value.split(',').all(private_property),
        _ => return None,
    })
}

const COLUMN_AVAILABILITY: &[&str] = &[
    "reply_time",
    "wal_status",
    "safe_wal_size",
    "two_phase",
    "two_phase_at",
    "conflicting",
    "inactive_since",
    "invalidation_reason",
    "failover",
    "synced",
    "receiver_received_lsn",
    "written_lsn",
    "flushed_lsn",
    "replay_pause_state",
];

fn visibility(value: &str, fields: &[&str]) -> bool {
    serde_json::from_str::<BTreeMap<String, serde_json::Value>>(value).is_ok_and(|values| {
        values.iter().all(|(name, value)| fields.contains(&name.as_str()) && (value.is_boolean() || value.is_null()))
    })
}

fn postgres_position(value: &str) -> bool {
    value.split_once('/').is_some_and(|(high, low)| {
        [high, low]
            .into_iter()
            .all(|part| (1..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
    })
}

fn local_clock(value: &str) -> bool {
    ["%y%m%d %H:%M:%S", "%Y-%m-%d %H:%M:%S"]
        .into_iter()
        .any(|format| chrono::NaiveDateTime::parse_from_str(value, format).is_ok())
        || matches!(value, "000000 00:00:00" | "0000-00-00 00:00:00")
}

fn interval(value: &str) -> bool {
    let scalar = value.replace(" days ", " ").replace(" day ", " ");
    scalar.contains(':')
        && scalar.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b':' | b'.' | b' ' | b'-' | b'+'))
}
