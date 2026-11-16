use super::*;
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
            logical: boolean(set, "logical"),
            database: property(set, "database"),
            ..Replica::new(name, ReplicaRole::Replica)
        }),
        "slot" => Some(Replica {
            condition: match (ordinary_slot_consumer(set), boolean(set, "active")) {
                (false, _) => ReplicaCondition::Reported,
                (true, Some(true)) => ReplicaCondition::SlotActive,
                (true, Some(false)) => ReplicaCondition::SlotInactive,
                (true, None) => ReplicaCondition::Reported,
            },
            lag_bytes: number(set, "kept"),
            logical: match property(set, "slot_type").as_deref() {
                Some("logical") => Some(true),
                Some("physical") => Some(false),
                _ => None,
            },
            sync: property(set, "slot_type"),
            inactive_seconds: ordinary_slot_consumer(set).then(|| number(set, "inactive_s")).flatten(),
            wal_status: property(set, "wal_status"),
            safe_wal_bytes: number(set, "safe_wal_size"),
            database: property(set, "database"),
            ..Replica::new(name, ReplicaRole::Slot)
        }),
        "standby" => Some(standby_replica(Standby {
            behind: number(set, "behind"),
            receiver: property(set, "receiver"),
            receiver_presence: receiver_presence(set),
            streaming_configured: boolean(set, "streaming_configured"),
            delay: number(set, "delay_s"),
        })),
        _ => None,
    }
}

/// What a standby reads about itself.
pub(super) struct Standby {
    /// WAL received and not yet replayed; `None` when nothing streams to it (archive recovery).
    pub(super) behind: Option<f64>,
    /// pg_stat_wal_receiver's status, when a receiver runs.
    pub(super) receiver: Option<String>,
    pub(super) receiver_presence: ReceiverPresence,
    /// Whether primary_conninfo is set: `None` when this login can't see it (it can hold a password) or the version
    /// keeps it in recovery.conf.
    pub(super) streaming_configured: Option<bool>,
    /// recovery_min_apply_delay, in seconds.
    pub(super) delay: Option<f64>,
}

pub(super) enum ReceiverPresence {
    Present,
    Absent,
    Unknown,
}

fn receiver_presence(set: &NativeCounterSet) -> ReceiverPresence {
    // Without pg_read_all_stats the native function retains the real PID but hides status and other details.
    // https://github.com/postgres/postgres/blob/REL_17_STABLE/src/backend/replication/walreceiver.c
    if set.properties.contains_key("active_pid") {
        ReceiverPresence::Present
    } else if boolean(set, "full_visibility") == Some(true) {
        ReceiverPresence::Absent
    } else {
        ReceiverPresence::Unknown
    }
}

/// Native receive/replay distance is retained independently from the age of the last replayed commit.
/// A long open transaction or idle publisher can make commit age large without measuring apply latency.
/// https://www.postgresql.org/docs/17/functions-admin.html#FUNCTIONS-RECOVERY-INFO
pub(super) fn standby_replica(s: Standby) -> Replica {
    let delay = s.delay.filter(|d| *d > 0.0);
    // A reported status remains positive evidence. Missing or hidden status does not establish receiver absence.
    let (condition, state) = match (s.receiver, s.receiver_presence, s.streaming_configured) {
        (Some(status), _, _) if status.eq_ignore_ascii_case("stopped") => (ReplicaCondition::Stopped, Some(status)),
        (Some(status), _, _) => (ReplicaCondition::Reported, Some(status)),
        (None, ReceiverPresence::Absent, Some(true)) => (ReplicaCondition::NotReceiving, None),
        (None, ReceiverPresence::Absent, Some(false)) => (ReplicaCondition::RestoringFromArchive, None),
        (None, ReceiverPresence::Absent, None) => (ReplicaCondition::NotStreaming, None),
        (None, ReceiverPresence::Present | ReceiverPresence::Unknown, _) => (ReplicaCondition::Reported, None),
    };
    Replica {
        condition,
        state,
        lag_seconds: None,
        lag_bytes: s.behind,
        delay_seconds: delay,
        ..Replica::new("this server", ReplicaRole::ThisServer)
    }
}

fn boolean(set: &NativeCounterSet, name: &str) -> Option<bool> {
    match set.properties.get(name).map(String::as_str) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

fn ordinary_slot_consumer(set: &NativeCounterSet) -> bool {
    match boolean(set, "synced") {
        Some(synced) => !synced,
        None => {
            set.properties
                .get("column_availability")
                .and_then(|value| serde_json::from_str::<BTreeMap<String, Value>>(value).ok())
                .and_then(|columns| columns.get("synced").and_then(Value::as_bool))
                == Some(false)
        }
    }
}
