//! Retain source-native positions and clocks exactly; measurements never create a consumer or source owner.
use super::*;

pub(crate) fn decode(row: &Row) -> Result<ReplicationObservation> {
    let source = text(row, "native_source")?;
    let values: BTreeMap<String, Value> = serde_json::from_str(&text(row, "native_values")?).map_err(|_| invalid())?;
    validate_source(&source, &values)?;
    let mut identity = NativeIdentity::new(&source).qualified("native_id", text(row, "native_id")?);
    let mut properties = BTreeMap::new();
    for name in [
        "kind",
        "name",
        "slot_type",
        "active",
        "database",
        "database_oid",
        "xmin",
        "catalog_xmin",
        "restart_lsn",
        "confirmed_flush_lsn",
        "wal_status",
        "invalidation_reason",
        "failover",
        "synced",
        "backend_start",
        "active_pid",
        "slot_name",
        "backend_xmin",
        "state",
        "sync_state",
        "logical",
        "sent_lsn",
        "write_lsn",
        "flush_lsn",
        "replay_lsn",
        "receive_lsn",
        "receiver",
        "streaming_configured",
        "current_lsn",
        "sync_priority",
        "reply_time",
        "write_lag",
        "flush_lag",
        "replay_lag",
        "plugin",
        "temporary",
        "two_phase",
        "two_phase_at",
        "conflicting",
        "inactive_since",
        "last_replay_commit_at",
        "receive_start_lsn",
        "receive_start_tli",
        "receiver_received_lsn",
        "written_lsn",
        "flushed_lsn",
        "received_tli",
        "last_msg_send_time",
        "last_msg_receipt_time",
        "latest_end_lsn",
        "latest_end_time",
        "replay_paused",
        "replay_pause_requested",
        "replay_pause_state",
        "replay_pause_access",
        "column_availability",
        "server_version_num",
        "read_database_oid",
        "read_standby",
        "observed_at",
        "full_visibility",
    ] {
        let Some(value) = values.get(name).filter(|value| !value.is_null()) else { continue };
        let value = match value {
            Value::String(value) => value.clone(),
            value => serde_json::to_string(value).map_err(|_| invalid())?,
        };
        if is_clock(name) {
            canonical_time(&value)?;
        }
        if is_position(name) && !valid_lsn(&value) {
            return Err(invalid());
        }
        if name == "backend_start" {
            identity = identity.qualified("backend_start", &value);
        }
        properties.insert(name.into(), value);
    }
    // Logical slot database labels qualify the decoded workload, not ownership of server-wide WAL positions.
    let scope = MeasurementScope::Server;
    let mut counters = Vec::new();
    for (name, unit, integer) in [
        ("behind", CounterUnit::Bytes, true),
        ("kept", CounterUnit::Bytes, true),
        ("safe_wal_size", CounterUnit::Bytes, true),
        ("lag_s", CounterUnit::Seconds, false),
        ("write_lag_s", CounterUnit::Seconds, false),
        ("flush_lag_s", CounterUnit::Seconds, false),
        ("inactive_s", CounterUnit::Seconds, false),
        ("since_replay", CounterUnit::Seconds, false),
        ("delay_s", CounterUnit::Seconds, false),
    ] {
        let value = match values.get(name) {
            None | Some(Value::Null) => continue,
            Some(Value::String(value)) => value,
            _ => return Err(invalid()),
        };
        if value.starts_with('-') {
            let valid = if integer {
                value.parse::<i128>().is_ok_and(|number| number < 0 && number.to_string() == *value)
            } else {
                value.parse::<f64>().is_ok_and(|number| number.is_finite() && number < 0.0)
            };
            if !valid {
                return Err(invalid());
            }
            properties.insert(format!("{name}_native"), value.clone());
            continue;
        }
        let value = if integer {
            let number = value.parse::<u64>().map_err(|_| invalid())?;
            if number.to_string() != *value {
                return Err(invalid());
            }
            MeasurementValue::Integer(value.clone())
        } else {
            MeasurementValue::Real(
                value.parse::<f64>().ok().filter(|value| value.is_finite() && *value >= 0.0).ok_or_else(invalid)?,
            )
        };
        counters.push(NativeCounterSample {
            key: name.into(),
            value,
            unit,
            nature: CounterNature::Gauge,
            scope,
            reset: culprit_core::ResetMarker::Unknown,
            native_object: source.clone(),
            native_name: name.into(),
            native_instance: None,
            native_type: None,
        });
    }
    let session_id = properties.get("active_pid").cloned();
    if let Some(id) = &session_id
        && !id.parse::<u32>().is_ok_and(|value| value > 0 && value <= i32::MAX as u32 && value.to_string() == *id)
    {
        return Err(invalid());
    }
    let session = session_id
        .as_ref()
        .zip(properties.get("backend_start"))
        .map(|(id, generation)| NativeActivityIdentity { id: id.clone(), generation: generation.clone() });
    let unresolved_session_id = session_id.filter(|_| session.is_none());
    let slot_name = properties.get("slot_name").cloned();
    let mut sample = NativeReplicationSample::new(NativeCounterSet {
        ratios: Vec::new(),
        ratio_gaps: Vec::new(),
        identity,
        scope,
        counters,
        properties,
    })?;
    sample.session = session;
    sample.unresolved_session_id = unresolved_session_id;
    sample.slot_name = slot_name;
    if !sample.is_valid() {
        return Err(invalid());
    }
    Ok(ReplicationObservation { source, fact: ReplicationFact::Native(Box::new(sample)) })
}

fn validate_source(source: &str, values: &BTreeMap<String, Value>) -> Result<()> {
    let kind = values.get("kind").and_then(Value::as_str).ok_or_else(invalid)?;
    if !matches!(
        (source, kind),
        ("pg_recovery", "standby") | ("pg_stat_replication", "sender") | ("pg_replication_slots", "slot")
    ) {
        return Err(invalid());
    }
    Ok(())
}

fn is_clock(name: &str) -> bool {
    matches!(
        name,
        "backend_start"
            | "reply_time"
            | "inactive_since"
            | "last_replay_commit_at"
            | "last_msg_send_time"
            | "last_msg_receipt_time"
            | "latest_end_time"
            | "observed_at"
    )
}

fn is_position(name: &str) -> bool {
    matches!(
        name,
        "restart_lsn"
            | "confirmed_flush_lsn"
            | "current_lsn"
            | "sent_lsn"
            | "write_lsn"
            | "flush_lsn"
            | "replay_lsn"
            | "receive_lsn"
            | "two_phase_at"
            | "receive_start_lsn"
            | "receiver_received_lsn"
            | "written_lsn"
            | "flushed_lsn"
            | "latest_end_lsn"
    )
}

fn valid_lsn(value: &str) -> bool {
    let Some((high, low)) = value.split_once('/') else { return false };
    [high, low]
        .into_iter()
        .all(|word| !word.is_empty() && word.len() <= 8 && word.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
