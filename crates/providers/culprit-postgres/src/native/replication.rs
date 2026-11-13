//! Native replication state and measurements; connection strings and WAL-receiver credentials are never selected.
use super::*;
use culprit_core::{MeasurementScope, Replica, ReplicaCondition, ReplicaRole, telemetry::*};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) fn sources(standby: bool) -> Vec<String> {
    if standby {
        vec!["pg_recovery".into(), "pg_replication_slots".into()]
    } else {
        vec!["pg_stat_replication".into(), "pg_replication_slots".into()]
    }
}
pub(crate) fn query(version: u32, standby: bool, database_oid: &str, limit: usize) -> String {
    let position = if standby { "pg_catalog.pg_last_wal_replay_lsn()" } else { "pg_catalog.pg_current_wal_lsn()" };
    let wal_status = if version >= 130_000 { "s.wal_status" } else { "NULL::text" };
    let safe_wal = if version >= 130_000 { "s.safe_wal_size" } else { "NULL::bigint" };
    let inactive = if version >= 170_000 {
        "extract(epoch FROM pg_catalog.clock_timestamp()-s.inactive_since)"
    } else {
        "NULL::numeric"
    };
    let slots = format!(
        "SELECT 'pg_replication_slots'::text AS native_source,s.slot_name::text AS native_id,
        pg_catalog.jsonb_build_object('kind','slot','name',s.slot_name,'slot_type',s.slot_type,'active',s.active,
        'database',s.database,'database_oid',s.datoid::text,'xmin',s.xmin::text,'catalog_xmin',s.catalog_xmin::text,
        'slot_name',s.slot_name,'active_pid',s.active_pid::text,'backend_start',{},
        'restart_lsn',s.restart_lsn::text,'confirmed_flush_lsn',s.confirmed_flush_lsn::text,
        'wal_status',{wal_status},'safe_wal_size',({safe_wal})::text,'inactive_s',({inactive})::text,
        'invalidation_reason',pg_catalog.to_jsonb(s)->>'invalidation_reason',
        'failover',pg_catalog.to_jsonb(s)->>'failover','synced',pg_catalog.to_jsonb(s)->>'synced',
        'kept',pg_catalog.pg_wal_lsn_diff({position},s.restart_lsn)::text)::text AS native_values
        FROM pg_catalog.pg_replication_slots s LEFT JOIN pg_catalog.pg_stat_replication r ON r.pid=s.active_pid
        WHERE s.datoid IS NULL OR s.datoid={database_oid}",
        iso("r.backend_start")
    );
    let endpoint = if standby {
        "SELECT 'pg_recovery'::text AS native_source,'local'::text AS native_id,
        pg_catalog.jsonb_build_object('kind','standby','name','this server',
        'receive_lsn',pg_catalog.pg_last_wal_receive_lsn()::text,'replay_lsn',pg_catalog.pg_last_wal_replay_lsn()::text,
        'receiver',(SELECT status FROM pg_catalog.pg_stat_wal_receiver LIMIT 1),
        'streaming_configured',(SELECT setting<>'' FROM pg_catalog.pg_settings WHERE name='primary_conninfo'),
        'delay_s',(SELECT setting::numeric/1000 FROM pg_catalog.pg_settings WHERE name='recovery_min_apply_delay')::text,
        'behind',pg_catalog.pg_wal_lsn_diff(pg_catalog.pg_last_wal_receive_lsn(),pg_catalog.pg_last_wal_replay_lsn())::text,
        'since_replay',extract(epoch FROM pg_catalog.clock_timestamp()-pg_catalog.pg_last_xact_replay_timestamp())::text)::text AS native_values".to_owned()
    } else {
        format!("SELECT 'pg_stat_replication'::text AS native_source,r.pid::text AS native_id,
        pg_catalog.jsonb_build_object('kind','sender','name',coalesce(nullif(r.application_name,''),r.client_addr::text,r.pid::text),
        'backend_start',{},'active_pid',r.pid::text,'slot_name',s.slot_name,'backend_xmin',r.backend_xmin::text,
        'state',r.state,'sync_state',r.sync_state,'database',s.database,
        'logical',coalesce(s.slot_type='logical',false),'sent_lsn',r.sent_lsn::text,'write_lsn',r.write_lsn::text,
        'flush_lsn',r.flush_lsn::text,'replay_lsn',r.replay_lsn::text,
        'lag_s',extract(epoch FROM r.replay_lag)::text,'behind',pg_catalog.pg_wal_lsn_diff(pg_catalog.pg_current_wal_lsn(),r.replay_lsn)::text)::text AS native_values
        FROM pg_catalog.pg_stat_replication r LEFT JOIN pg_catalog.pg_replication_slots s ON s.active_pid=r.pid",iso("r.backend_start"))
    };
    format!("SELECT * FROM ({endpoint} UNION ALL {slots}) native_replication LIMIT {limit}")
}
pub(crate) fn decode(row: &Row) -> Result<ReplicationObservation> {
    let source = text(row, "native_source")?;
    if !matches!(source.as_str(), "pg_recovery" | "pg_stat_replication" | "pg_replication_slots") {
        return Err(invalid());
    }
    let values: BTreeMap<String, Value> = serde_json::from_str(&text(row, "native_values")?).map_err(|_| invalid())?;
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
    ] {
        if let Some(value) = values.get(name).filter(|value| !value.is_null()) {
            let value = match value {
                Value::String(value) => value.clone(),
                value => serde_json::to_string(value).map_err(|_| invalid())?,
            };
            if name == "backend_start" {
                canonical_time(&value)?;
                identity = identity.qualified("backend_start", &value);
            }
            properties.insert(name.into(), value);
        }
    }
    // A logical slot's database identifies its decoded workload. Native WAL positions span the whole instance.
    let scope = MeasurementScope::Server;
    let mut counters = vec![];
    for (name, unit, integer) in [
        ("behind", CounterUnit::Bytes, true),
        ("kept", CounterUnit::Bytes, true),
        ("safe_wal_size", CounterUnit::Bytes, true),
        ("lag_s", CounterUnit::Seconds, false),
        ("inactive_s", CounterUnit::Seconds, false),
        ("since_replay", CounterUnit::Seconds, false),
        ("delay_s", CounterUnit::Seconds, false),
    ] {
        let Some(value) = values.get(name).and_then(Value::as_str) else {
            continue;
        };
        // Negative native lag/limits are retained as context, not turned into a measured zero.
        if value.starts_with('-') {
            properties.insert(format!("{name}_native"), value.into());
            continue;
        }
        let value = if integer {
            MeasurementValue::Integer(value.parse::<u64>().map_err(|_| invalid())?.to_string())
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
    if let Some(id) = &session_id {
        id.parse::<u32>().map_err(|_| invalid())?;
    }
    let session = session_id
        .as_ref()
        .zip(properties.get("backend_start"))
        .map(|(id, generation)| NativeActivityIdentity { id: id.clone(), generation: generation.clone() });
    let unresolved_session_id = session_id.filter(|_| session.is_none());
    let slot_name = properties.get("slot_name").cloned();
    let measurements = NativeCounterSet { identity, scope, counters, properties };
    let mut sample = NativeReplicationSample::new(measurements)?;
    sample.session = session;
    sample.unresolved_session_id = unresolved_session_id;
    sample.slot_name = slot_name;
    if !sample.is_valid() {
        return Err(invalid());
    }
    let fact = ReplicationFact::Native(Box::new(sample));
    Ok(ReplicationObservation { source, fact })
}
fn number(set: &NativeCounterSet, name: &str) -> Option<f64> {
    match &set.counters.iter().find(|value| value.key == name)?.value {
        MeasurementValue::Integer(value) => value.parse().ok(),
        MeasurementValue::Real(value) => Some(*value),
    }
}
fn property(set: &NativeCounterSet, name: &str) -> Option<String> {
    set.properties.get(name).cloned()
}
pub(crate) fn project(fact: &ReplicationFact) -> Option<Replica> {
    let ReplicationFact::Native(sample) = fact else {
        return None;
    };
    let set = &sample.measurements;
    let name = property(set, "name")?;
    match property(set, "kind")?.as_str() {
        "sender" => Some(Replica {
            state: property(set, "state"),
            sync: property(set, "sync_state"),
            lag_seconds: number(set, "lag_s"),
            lag_bytes: number(set, "behind"),
            logical: property(set, "logical").map(|value| value == "true"),
            database: property(set, "database"),
            ..Replica::new(name, ReplicaRole::Replica)
        }),
        "slot" => Some(Replica {
            condition: if property(set, "active").as_deref() == Some("true") {
                ReplicaCondition::SlotActive
            } else {
                ReplicaCondition::SlotInactive
            },
            lag_bytes: number(set, "kept"),
            logical: property(set, "slot_type").map(|value| value == "logical"),
            sync: property(set, "slot_type"),
            inactive_seconds: number(set, "inactive_s"),
            wal_status: property(set, "wal_status"),
            safe_wal_bytes: number(set, "safe_wal_size"),
            database: property(set, "database"),
            ..Replica::new(name, ReplicaRole::Slot)
        }),
        "standby" => Some(standby_replica(Standby {
            behind: number(set, "behind"),
            since_replay: number(set, "since_replay"),
            receiver: property(set, "receiver"),
            streaming_configured: property(set, "streaming_configured").map(|value| value == "true"),
            delay: number(set, "delay_s"),
        })),
        _ => None,
    }
}

/// What a standby reads about itself.
struct Standby {
    /// WAL received and not yet replayed; `None` when nothing streams to it (archive recovery).
    behind: Option<f64>,
    /// Seconds since the last replayed commit was committed on the primary.
    since_replay: Option<f64>,
    /// pg_stat_wal_receiver's status, when a receiver runs.
    receiver: Option<String>,
    /// Whether primary_conninfo is set: `None` when this login can't see it (it can hold a password) or the version
    /// keeps it in recovery.conf.
    streaming_configured: Option<bool>,
    /// recovery_min_apply_delay, in seconds.
    delay: Option<f64>,
}

/// This server's own lag as a standby. Seconds since the last replayed commit are lag only while there is WAL it
/// received and hasn't replayed: on a quiet primary they grow with nothing to apply. A standby without a WAL receiver
/// is stopped only when it is meant to stream (primary_conninfo set); without it, it restores WAL from an archive.
fn standby_replica(s: Standby) -> Replica {
    let delay = s.delay.filter(|d| *d > 0.0);
    // The WAL receiver's own status is the engine's word; without a receiver, how it stands is a fact the catalog words.
    let (condition, state) = match (s.receiver, s.streaming_configured) {
        (Some(status), _) if status.eq_ignore_ascii_case("stopped") => (ReplicaCondition::Stopped, Some(status)),
        (Some(status), _) => (ReplicaCondition::Reported, Some(status)),
        (None, Some(true)) => (ReplicaCondition::NotReceiving, None),
        (None, Some(false)) => (ReplicaCondition::RestoringFromArchive, None),
        (None, None) => (ReplicaCondition::NotStreaming, None),
    };
    let lag_seconds = match s.behind {
        Some(b) if b >= culprit_core::signals::STANDBY_BEHIND_BYTES => {
            s.since_replay.map(|t| (t - delay.unwrap_or(0.0)).max(0.0))
        }
        Some(_) => Some(0.0),
        None => None,
    };
    Replica {
        condition,
        state,
        lag_seconds,
        lag_bytes: s.behind,
        delay_seconds: delay,
        ..Replica::new("this server", ReplicaRole::ThisServer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn standby(behind: Option<f64>, since_replay: f64) -> Standby {
        Standby {
            behind,
            since_replay: Some(since_replay),
            receiver: Some("streaming".into()),
            streaming_configured: Some(true),
            delay: None,
        }
    }

    // On a quiet primary the last replayed commit gets older by the minute while there is nothing to replay: that is
    // the primary's silence, not lag.
    #[test]
    fn a_standby_with_nothing_to_replay_is_not_behind() {
        let idle = standby_replica(standby(Some(0.0), 3_600.0));
        assert_eq!((idle.lag_seconds, idle.state.as_deref()), (Some(0.0), Some("streaming")));
        let behind = standby_replica(standby(Some(64.0 * 1024.0 * 1024.0), 95.0));
        assert_eq!(behind.lag_seconds, Some(95.0));
        // The false flag: the primary ran one long CREATE INDEX for an hour without a commit, and the standby, current
        // with its replay, was a few kilobytes behind when the scan ran. That hour is the primary's, not lag.
        let current = standby_replica(standby(Some(8_192.0), 3_600.0));
        assert_eq!(current.lag_seconds, Some(0.0));
    }

    // recovery_min_apply_delay holds replay back on purpose: only what is beyond it is lag.
    #[test]
    fn a_delayed_standby_counts_only_the_lag_beyond_its_delay() {
        let delayed = standby_replica(Standby { delay: Some(3_600.0), ..standby(Some(1e9), 3_640.0) });
        assert_eq!((delayed.lag_seconds, delayed.delay_seconds), (Some(40.0), Some(3_600.0)));
    }

    // Without a WAL receiver a standby is stopped only when it is meant to stream; log shipping restores from an
    // archive and has no receiver at all.
    #[test]
    fn a_standby_without_a_receiver_is_stopped_only_when_it_should_stream() {
        let none = |configured| Standby {
            behind: None,
            receiver: None,
            streaming_configured: configured,
            ..standby(None, 600.0)
        };
        let stopped = standby_replica(none(Some(true)));
        assert_eq!(stopped.condition, ReplicaCondition::NotReceiving);
        assert!(stopped.condition.is_stopped());
        assert_eq!(stopped.state_words().as_deref(), Some("not receiving"));
        let shipping = standby_replica(none(Some(false)));
        assert_eq!((shipping.condition, shipping.lag_seconds), (ReplicaCondition::RestoringFromArchive, None));
        assert!(!shipping.condition.is_stopped());
        assert_eq!(shipping.state_words().as_deref(), Some("restoring from archive"));
        assert_eq!(standby_replica(none(None)).state_words().as_deref(), Some("not streaming"));
        // A receiver's own status is the engine's word: "stopped" is a stop, "streaming" isn't.
        let receiving = |status: &str| Standby { receiver: Some(status.into()), ..none(Some(true)) };
        assert_eq!(standby_replica(receiving("stopped")).condition, ReplicaCondition::Stopped);
        assert_eq!(standby_replica(receiving("streaming")).state_words().as_deref(), Some("streaming"));
    }

    #[test]
    fn native_slot_columns_follow_available_versions_without_selecting_connection_strings() {
        assert!(!query(120_000, false, "1", 1000).contains("s.wal_status"));
        assert!(query(130_000, false, "1", 1000).contains("s.safe_wal_size"));
        assert!(!query(160_000, false, "1", 1000).contains("s.inactive_since"));
        assert!(query(170_000, false, "1", 1000).contains("s.inactive_since"));
        assert!(!query(180_000, true, "1", 1000).contains("SELECT conninfo"));
    }

    #[test]
    fn logical_slot_wal_positions_keep_server_scope_and_generation_checked_consumer_links() {
        let mut values = serde_json::json!({
            "kind":"slot","name":"consumer","slot_name":"consumer","slot_type":"logical",
            "database_oid":"16384","database":"application","active":true,
            "active_pid":"42","backend_start":"2026-11-13T06:00:00.123456Z",
            "restart_lsn":"0/1234","kept":"9007199254740993"
        });
        let row = |values: &serde_json::Value| {
            Row::from_fields([
                ("native_source".into(), Some("pg_replication_slots".into())),
                ("native_id".into(), Some("consumer".into())),
                ("native_values".into(), Some(values.to_string())),
            ])
        };
        let decoded = decode(&row(&values)).unwrap();
        let ReplicationFact::Native(sample) = decoded.fact else { panic!() };
        assert_eq!(sample.measurements.scope, MeasurementScope::Server);
        assert!(sample.measurements.counters.iter().all(|counter| counter.scope == MeasurementScope::Server));
        assert_eq!(sample.measurements.counters[0].value.integer(), Some(9_007_199_254_740_993));
        assert_eq!(sample.session.as_ref().unwrap().generation, "2026-11-13T06:00:00.123456Z");
        assert_eq!(sample.slot_name.as_deref(), Some("consumer"));
        values["backend_start"] = serde_json::Value::Null;
        let decoded = decode(&row(&values)).unwrap();
        let ReplicationFact::Native(unresolved) = decoded.fact else { panic!() };
        assert!(unresolved.session.is_none());
        assert_eq!(unresolved.unresolved_session_id.as_deref(), Some("42"));
    }
}
