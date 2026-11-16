//! PostgreSQL positions, acknowledgement intervals and slot states retain their separate native meanings.
use super::support::{counter, entry, measurement, native, property};
use culprit_core::{
    MeasurementScope, ReplicationCondition, ReplicationEntry, ReplicationField, ReplicationFieldGroup, t, telemetry::*,
};

#[derive(Clone, Copy)]
enum Family {
    Sender,
    Standby,
    Slot,
}

impl Family {
    fn of(row: &ReplicationObservation, sample: &NativeReplicationSample) -> Option<Self> {
        if sample.measurements.scope != MeasurementScope::Server || !matches!(&row.fact, ReplicationFact::Native(_)) {
            return None;
        }
        match (row.source.as_str(), property(sample, "kind")?) {
            ("pg_stat_replication", "sender") => Some(Self::Sender),
            ("pg_recovery", "standby") => Some(Self::Standby),
            ("pg_replication_slots", "slot") => Some(Self::Slot),
            _ => None,
        }
    }
}

/// Native state classification is cheap enough to inspect every retained point before limiting presentation.
pub(super) fn conditions(
    row: &ReplicationObservation,
    sample: &NativeReplicationSample,
) -> Option<Vec<ReplicationCondition>> {
    let mut conditions = Vec::new();
    match Family::of(row, sample)? {
        Family::Sender => {}
        Family::Standby => {
            if property(sample, "receiver") == Some("stopped") {
                conditions.push(ReplicationCondition::ReceiverStopped);
            }
            if replay_is_paused(sample) {
                conditions.push(ReplicationCondition::ReplayPaused);
            }
        }
        Family::Slot => {
            if property(sample, "wal_status") == Some("lost")
                || property(sample, "invalidation_reason").is_some_and(|reason| !reason.is_empty())
                || property(sample, "conflicting") == Some("true")
            {
                conditions.push(ReplicationCondition::SlotInvalid);
            }
            if property(sample, "wal_status") == Some("unreserved") {
                conditions.push(ReplicationCondition::WalUnreserved);
            }
        }
    }
    Some(conditions)
}

pub(super) fn explain(row: &ReplicationObservation, sample: &NativeReplicationSample) -> Option<ReplicationEntry> {
    if !matches!(&row.fact, ReplicationFact::Native(original) if original.as_ref() == sample) {
        return None;
    }
    let family = Family::of(row, sample)?;
    let mut result = match family {
        Family::Sender => sender(row, sample),
        Family::Standby => standby(row, sample),
        Family::Slot => slot(row, sample),
    };
    result.conditions = conditions(row, sample)?;
    result.session = sample.session.clone();
    result.unresolved_session_id = sample.unresolved_session_id.clone();
    result.groups.push(observation_context(sample));
    Some(result)
}

fn sender(row: &ReplicationObservation, sample: &NativeReplicationSample) -> ReplicationEntry {
    let mut result = entry(
        row,
        sample.measurements.scope,
        t!("view.replication.postgres.sender.title"),
        t!("view.replication.postgres.sender.explanation"),
    );
    result.fields = vec![
        field(sample, "name", t!("view.replication.postgres.name")),
        field(sample, "state", t!("view.replication.postgres.sender_state")),
        field(sample, "sync_state", t!("view.replication.postgres.sync_state")),
        field(sample, "sync_priority", t!("view.replication.postgres.sync_priority")),
        field(sample, "database", t!("view.replication.postgres.database")),
        field(sample, "database_oid", t!("view.replication.postgres.database_oid")),
        field(sample, "backend_xmin", t!("view.replication.postgres.backend_xmin")),
    ];
    result.groups = vec![
        group(
            t!("view.replication.postgres.positions"),
            vec![
                field(sample, "current_lsn", t!("view.replication.postgres.current_lsn")),
                field(sample, "sent_lsn", t!("view.replication.postgres.sent_lsn")),
                field(sample, "write_lsn", t!("view.replication.postgres.write_lsn")),
                field(sample, "flush_lsn", t!("view.replication.postgres.flush_lsn")),
                field(sample, "replay_lsn", t!("view.replication.postgres.replay_lsn")),
                distance(sample, "behind", t!("view.replication.postgres.sender_distance")),
            ],
        ),
        group(
            t!("view.replication.postgres.acknowledgements"),
            vec![
                field(sample, "write_lag", t!("view.replication.postgres.write_lag")),
                field(sample, "flush_lag", t!("view.replication.postgres.flush_lag")),
                field(sample, "replay_lag", t!("view.replication.postgres.replay_lag")),
                field(sample, "reply_time", t!("view.replication.postgres.reply_time")),
            ],
        ),
    ];
    result.investigation = t!("view.replication.postgres.sender.investigate");
    result.verify = t!("view.replication.postgres.sender.verify");
    result
}

fn standby(row: &ReplicationObservation, sample: &NativeReplicationSample) -> ReplicationEntry {
    let mut result = entry(
        row,
        sample.measurements.scope,
        t!("view.replication.postgres.standby.title"),
        t!("view.replication.postgres.standby.explanation"),
    );
    result.fields = vec![
        field(sample, "receiver", t!("view.replication.postgres.receiver_state")),
        field(sample, "slot_name", t!("view.replication.postgres.slot_name")),
        field(sample, "streaming_configured", t!("view.replication.postgres.streaming_configured")),
        field(sample, "replay_pause_state", t!("view.replication.postgres.replay_pause_state")),
        field(sample, "replay_pause_requested", t!("view.replication.postgres.replay_pause_requested")),
        field(sample, "replay_paused", t!("view.replication.postgres.replay_paused")),
        counter("delay_s", t!("view.replication.postgres.apply_delay"), measurement(sample, "delay_s")),
    ];
    result.groups = vec![
        group(
            t!("view.replication.postgres.positions"),
            vec![
                field(sample, "receive_start_lsn", t!("view.replication.postgres.receive_start_lsn")),
                field(sample, "receive_start_tli", t!("view.replication.postgres.receive_start_tli")),
                field(sample, "receive_lsn", t!("view.replication.postgres.receive_lsn")),
                field(sample, "receiver_received_lsn", t!("view.replication.postgres.receiver_received_lsn")),
                field(sample, "written_lsn", t!("view.replication.postgres.written_lsn")),
                field(sample, "flushed_lsn", t!("view.replication.postgres.flushed_lsn")),
                field(sample, "received_tli", t!("view.replication.postgres.received_tli")),
                field(sample, "replay_lsn", t!("view.replication.postgres.replay_lsn")),
                field(sample, "latest_end_lsn", t!("view.replication.postgres.latest_end_lsn")),
                distance(sample, "behind", t!("view.replication.postgres.apply_distance")),
            ],
        ),
        group(
            t!("view.replication.postgres.native_clocks"),
            vec![
                field(sample, "last_msg_send_time", t!("view.replication.postgres.last_msg_send_time")),
                field(sample, "last_msg_receipt_time", t!("view.replication.postgres.last_msg_receipt_time")),
                field(sample, "latest_end_time", t!("view.replication.postgres.latest_end_time")),
                field(sample, "last_replay_commit_at", t!("view.replication.postgres.last_replay_commit_at")),
                counter(
                    "since_replay",
                    t!("view.replication.postgres.since_replay"),
                    measurement(sample, "since_replay"),
                ),
            ],
        ),
    ];
    result.investigation = t!("view.replication.postgres.standby.investigate");
    result.verify = t!("view.replication.postgres.standby.verify");
    result
}

fn slot(row: &ReplicationObservation, sample: &NativeReplicationSample) -> ReplicationEntry {
    let synced = property(sample, "synced") == Some("true");
    let mut result = entry(
        row,
        sample.measurements.scope,
        t!("view.replication.postgres.slot.title"),
        if synced {
            t!("view.replication.postgres.slot.synced_explanation")
        } else {
            t!("view.replication.postgres.slot.explanation")
        },
    );
    result.fields = vec![
        field(sample, "slot_name", t!("view.replication.postgres.slot_name")),
        field(sample, "slot_type", t!("view.replication.postgres.slot_type")),
        field(sample, "plugin", t!("view.replication.postgres.plugin")),
        field(sample, "temporary", t!("view.replication.postgres.temporary")),
        field(sample, "active", t!("view.replication.postgres.active")),
        field(sample, "database", t!("view.replication.postgres.database")),
        field(sample, "database_oid", t!("view.replication.postgres.database_oid")),
    ];
    result.groups = vec![
        group(
            t!("view.replication.postgres.retention"),
            vec![
                field(sample, "restart_lsn", t!("view.replication.postgres.restart_lsn")),
                field(sample, "confirmed_flush_lsn", t!("view.replication.postgres.confirmed_flush_lsn")),
                distance(sample, "kept", t!("view.replication.postgres.kept")),
                field(sample, "wal_status", t!("view.replication.postgres.wal_status")),
                counter(
                    "safe_wal_size",
                    t!("view.replication.postgres.safe_wal_size"),
                    measurement(sample, "safe_wal_size"),
                ),
                field(sample, "xmin", t!("view.replication.postgres.xmin")),
                field(sample, "catalog_xmin", t!("view.replication.postgres.catalog_xmin")),
            ],
        ),
        group(
            t!("view.replication.postgres.slot_readiness"),
            vec![
                field(sample, "conflicting", t!("view.replication.postgres.conflicting")),
                field(sample, "invalidation_reason", t!("view.replication.postgres.invalidation_reason")),
                field(sample, "failover", t!("view.replication.postgres.failover")),
                field(sample, "synced", t!("view.replication.postgres.synced")),
                field(sample, "two_phase", t!("view.replication.postgres.two_phase")),
                field(sample, "two_phase_at", t!("view.replication.postgres.two_phase_at")),
                field(
                    sample,
                    "inactive_since",
                    if synced {
                        t!("view.replication.postgres.sync_stopped_at")
                    } else {
                        t!("view.replication.postgres.inactive_since")
                    },
                ),
                counter(
                    "inactive_s",
                    if synced {
                        t!("view.replication.postgres.sync_stopped_age")
                    } else {
                        t!("view.replication.postgres.inactive_s")
                    },
                    measurement(sample, "inactive_s"),
                ),
            ],
        ),
    ];
    result.investigation = t!("view.replication.postgres.slot.investigate");
    result.verify = t!("view.replication.postgres.slot.verify");
    result
}

fn observation_context(sample: &NativeReplicationSample) -> ReplicationFieldGroup {
    let mut fields = vec![
        field(sample, "active_pid", t!("view.replication.postgres.active_pid")),
        field(sample, "backend_start", t!("view.replication.postgres.backend_start")),
        field(sample, "observed_at", t!("view.replication.postgres.observed_at")),
        field(sample, "server_version_num", t!("view.replication.postgres.server_version_num")),
        field(sample, "full_visibility", t!("view.replication.postgres.full_visibility")),
    ];
    // Unsupported columns, supported native NULLs and denied functions remain distinct facts.
    match property(sample, "kind") {
        Some("sender") => {
            fields.push(column(sample, "reply_time", t!("view.replication.postgres.reply_time_available")))
        }
        Some("standby") => fields.extend([
            column(sample, "receiver_received_lsn", t!("view.replication.postgres.receiver_received_lsn_available")),
            column(sample, "written_lsn", t!("view.replication.postgres.written_lsn_available")),
            column(sample, "flushed_lsn", t!("view.replication.postgres.flushed_lsn_available")),
            column(sample, "replay_pause_state", t!("view.replication.postgres.replay_pause_state_available")),
            access(sample, "paused", t!("view.replication.postgres.replay_paused_access")),
            access(sample, "requested", t!("view.replication.postgres.replay_pause_requested_access")),
            access(sample, "state", t!("view.replication.postgres.replay_pause_state_access")),
        ]),
        Some("slot") => fields.extend([
            column(sample, "wal_status", t!("view.replication.postgres.wal_status_available")),
            column(sample, "safe_wal_size", t!("view.replication.postgres.safe_wal_size_available")),
            column(sample, "two_phase", t!("view.replication.postgres.two_phase_available")),
            column(sample, "two_phase_at", t!("view.replication.postgres.two_phase_at_available")),
            column(sample, "conflicting", t!("view.replication.postgres.conflicting_available")),
            column(sample, "inactive_since", t!("view.replication.postgres.inactive_since_available")),
            column(sample, "invalidation_reason", t!("view.replication.postgres.invalidation_reason_available")),
            column(sample, "failover", t!("view.replication.postgres.failover_available")),
            column(sample, "synced", t!("view.replication.postgres.synced_available")),
        ]),
        _ => {}
    }
    group(t!("view.replication.postgres.observation_context"), fields)
}

fn field(sample: &NativeReplicationSample, key: &str, label: String) -> ReplicationField {
    native(key, label, property(sample, key))
}

fn distance(sample: &NativeReplicationSample, key: &str, label: String) -> ReplicationField {
    match measurement(sample, key) {
        Some(value) => counter(key, label, Some(value)),
        None => native(key, label, property(sample, &format!("{key}_native"))),
    }
}

fn group(title: String, fields: Vec<ReplicationField>) -> ReplicationFieldGroup {
    ReplicationFieldGroup { title, fields }
}

fn column(sample: &NativeReplicationSample, key: &str, label: String) -> ReplicationField {
    native(
        &format!("column:{key}"),
        label,
        object_flag(sample, "column_availability", key).map(|available| {
            if available {
                t!("view.replication.postgres.supported")
            } else {
                t!("view.replication.postgres.unsupported")
            }
        }),
    )
}

fn access(sample: &NativeReplicationSample, key: &str, label: String) -> ReplicationField {
    native(
        &format!("access:{key}"),
        label,
        object_flag(sample, "replay_pause_access", key).map(|allowed| {
            if allowed { t!("view.replication.postgres.allowed") } else { t!("view.replication.postgres.denied") }
        }),
    )
}

fn object_flag(sample: &NativeReplicationSample, object: &str, field: &str) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(property(sample, object)?).ok()?.get(field)?.as_bool()
}

fn replay_is_paused(sample: &NativeReplicationSample) -> bool {
    property(sample, "replay_pause_state") == Some("paused")
        || (property(sample, "server_version_num")
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|version| (100_000..140_000).contains(&version))
            && property(sample, "replay_paused") == Some("true"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn row(
        source: &str,
        kind: &str,
        properties: &[(&str, &str)],
        counters: Vec<NativeCounterSample>,
    ) -> ReplicationObservation {
        let mut values = BTreeMap::from([("kind".into(), kind.into())]);
        values.extend(properties.iter().map(|(key, value)| (key.to_string(), value.to_string())));
        let sample = NativeReplicationSample::new(NativeCounterSet {
            identity: NativeIdentity::new(source).qualified("native_id", "original"),
            scope: MeasurementScope::Server,
            counters,
            properties: values,
        })
        .unwrap();
        ReplicationObservation { source: source.into(), fact: ReplicationFact::Native(Box::new(sample)) }
    }

    fn report(row: &ReplicationObservation) -> ReplicationEntry {
        let ReplicationFact::Native(sample) = &row.fact else { panic!() };
        explain(row, sample).unwrap()
    }

    fn fields(report: &ReplicationEntry) -> BTreeMap<&str, &ReplicationField> {
        report
            .fields
            .iter()
            .chain(report.groups.iter().flat_map(|group| &group.fields))
            .map(|field| (field.key.as_str(), field))
            .collect()
    }

    fn bytes(key: &str, value: u64) -> NativeCounterSample {
        NativeCounterSample::gauge(
            key,
            value,
            CounterUnit::Bytes,
            MeasurementScope::Server,
            "pg_replication_slots",
            key,
        )
    }

    #[test]
    fn only_original_known_native_source_and_row_kind_select_postgresql_meaning() {
        for (source, kind) in [("other_source", "slot"), ("pg_recovery", "sender"), ("pg_stat_replication", "unknown")]
        {
            let point = row(source, kind, &[], vec![]);
            let ReplicationFact::Native(sample) = &point.fact else { panic!() };
            assert!(explain(&point, sample).is_none());
            assert!(conditions(&point, sample).is_none());
        }
        let mut point = row("pg_recovery", "standby", &[], vec![]);
        let ReplicationFact::Native(sample) = &mut point.fact else { panic!() };
        sample.measurements.scope = MeasurementScope::Database;
        let ReplicationFact::Native(sample) = &point.fact else { panic!() };
        assert!(conditions(&point, sample).is_none());
        let original = row("pg_recovery", "standby", &[("receiver", "streaming")], vec![]);
        let different = row("pg_recovery", "standby", &[("receiver", "stopped")], vec![]);
        let ReplicationFact::Native(sample) = &different.fact else { panic!() };
        assert!(explain(&original, sample).is_none());
    }

    #[test]
    fn exact_slot_bytes_native_zero_and_missing_margin_do_not_infer_loss_or_healthy_storage() {
        let point = row(
            "pg_replication_slots",
            "slot",
            &[("restart_lsn", "FFFFFFFF/FFFFFFFE")],
            vec![bytes("kept", 9_007_199_254_740_993), bytes("safe_wal_size", 0)],
        );
        let result = report(&point);
        let values = fields(&result);
        assert_eq!(
            values["kept"].value.as_deref(),
            Some(culprit_core::format::digits("9007199254740993".into()).as_str())
        );
        assert_eq!(values["safe_wal_size"].value.as_deref(), Some("0"));
        assert_eq!(values["restart_lsn"].value.as_deref(), Some("FFFFFFFF/FFFFFFFE"));
        assert!(result.conditions.is_empty());
        let missing = report(&row("pg_replication_slots", "slot", &[], vec![bytes("kept", 0)]));
        assert!(fields(&missing)["safe_wal_size"].value.is_none());
        assert_eq!(missing.conditions, result.conditions);
        let negative = report(&row("pg_replication_slots", "slot", &[("kept_native", "-1")], vec![]));
        assert_eq!(fields(&negative)["kept"].value.as_deref(), Some("-1"));
        assert!(negative.conditions.is_empty());
    }

    #[test]
    fn only_native_slot_loss_and_unreserved_states_qualify_conditions() {
        for (properties, expected) in [
            (vec![("wal_status", "lost")], vec![ReplicationCondition::SlotInvalid]),
            (vec![("invalidation_reason", "idle_timeout")], vec![ReplicationCondition::SlotInvalid]),
            (vec![("conflicting", "true")], vec![ReplicationCondition::SlotInvalid]),
            (vec![("wal_status", "unreserved")], vec![ReplicationCondition::WalUnreserved]),
            (vec![("wal_status", "extended")], vec![]),
            (vec![("active", "false"), ("synced", "true")], vec![]),
        ] {
            assert_eq!(report(&row("pg_replication_slots", "slot", &properties, vec![])).conditions, expected);
        }
    }

    #[test]
    fn synchronized_slot_timestamp_is_sync_interruption_without_a_consumer_abandonment_claim() {
        let point = row(
            "pg_replication_slots",
            "slot",
            &[("synced", "true"), ("active", "false"), ("inactive_since", "2026-11-13T06:00:00.123456Z")],
            vec![],
        );
        let result = report(&point);
        assert_eq!(fields(&result)["inactive_since"].label, t!("view.replication.postgres.sync_stopped_at"));
        assert_eq!(result.explanation, t!("view.replication.postgres.slot.synced_explanation"));
        assert!(result.conditions.is_empty());
        let unknown =
            report(&row("pg_replication_slots", "slot", &[("inactive_since", "2026-11-13T06:00:00.123456Z")], vec![]));
        assert_eq!(fields(&unknown)["inactive_since"].label, t!("view.replication.postgres.inactive_since"));
    }

    #[test]
    fn pause_request_is_not_paused_and_legacy_boolean_requires_original_version_semantics() {
        for (properties, paused) in [
            (vec![("replay_pause_requested", "true"), ("replay_pause_state", "pause requested")], false),
            (vec![("replay_pause_state", "paused")], true),
            (vec![("server_version_num", "130000"), ("replay_paused", "true")], true),
            (vec![("server_version_num", "170000"), ("replay_paused", "true")], false),
            (vec![("replay_paused", "true")], false),
        ] {
            assert_eq!(
                report(&row("pg_recovery", "standby", &properties, vec![]))
                    .conditions
                    .contains(&ReplicationCondition::ReplayPaused),
                paused
            );
        }
    }

    #[test]
    fn unsupported_columns_native_null_and_denied_pause_access_remain_distinct() {
        let point = row(
            "pg_recovery",
            "standby",
            &[
                (
                    "column_availability",
                    r#"{"written_lsn":true,"receiver_received_lsn":false,"replay_pause_state":true}"#,
                ),
                ("replay_pause_access", r#"{"requested":true,"state":false}"#),
            ],
            vec![],
        );
        let result = report(&point);
        let values = fields(&result);
        assert!(values["written_lsn"].value.is_none());
        assert_eq!(values["column:written_lsn"].value, Some(t!("view.replication.postgres.supported")));
        assert_eq!(values["column:receiver_received_lsn"].value, Some(t!("view.replication.postgres.unsupported")));
        assert_eq!(values["access:state"].value, Some(t!("view.replication.postgres.denied")));
        assert_eq!(values["access:requested"].value, Some(t!("view.replication.postgres.allowed")));
        assert!(values["access:paused"].value.is_none());
        assert!(result.conditions.is_empty());
    }

    #[test]
    fn sender_intervals_and_native_clocks_retain_exact_spelling_without_query_attribution() {
        let mut point = row(
            "pg_stat_replication",
            "sender",
            &[
                ("write_lag", "00:00:00.123456"),
                ("reply_time", "2030-01-01T00:00:00.654321Z"),
                ("sent_lsn", "FFFFFFFF/FFFFFFFF"),
            ],
            vec![],
        );
        let ReplicationFact::Native(sample) = &mut point.fact else { panic!() };
        sample.unresolved_session_id = Some("42".into());
        let result = report(&point);
        let values = fields(&result);
        assert_eq!(values["write_lag"].value.as_deref(), Some("00:00:00.123456"));
        assert_eq!(values["reply_time"].value.as_deref(), Some("2030-01-01T00:00:00.654321Z"));
        assert_eq!(values["sent_lsn"].value.as_deref(), Some("FFFFFFFF/FFFFFFFF"));
        assert!(result.session.is_none());
        assert_eq!(result.unresolved_session_id.as_deref(), Some("42"));
        assert!(result.conditions.is_empty());
        let ReplicationFact::Native(sample) = &mut point.fact else { panic!() };
        sample.unresolved_session_id = None;
        sample.session =
            Some(NativeActivityIdentity { id: "42".into(), generation: "2026-11-13T06:00:00.123456Z".into() });
        let expected = sample.session.clone();
        assert_eq!(report(&point).session, expected);
    }
}
