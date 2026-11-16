use super::*;
use culprit_core::{ReplicationField, ResetMarker};
use std::collections::BTreeMap;

fn row(io: Option<&str>, sql: Option<&str>, counters: &[(&str, u64)]) -> ReplicationObservation {
    let mut properties: BTreeMap<String, String> =
        [("channel".into(), "orders".into()), ("source_host".into(), "source.example.invalid".into())].into();
    if let Some(io) = io {
        properties.insert("io_running".into(), io.into());
    }
    if let Some(sql) = sql {
        properties.insert("sql_running".into(), sql.into());
    }
    let measurements = NativeCounterSet {
        identity: NativeIdentity::new("\"orders\""),
        scope: MeasurementScope::Server,
        properties,
        counters: counters
            .iter()
            .map(|(key, value)| {
                let unit = match *key {
                    "received-source-log-position" | "executed-source-log-position" | "relay-log-position" => {
                        CounterUnit::Bytes
                    }
                    "lag-seconds" | "configured-delay" => CounterUnit::Seconds,
                    _ => CounterUnit::Count,
                };
                NativeCounterSample::gauge(key, *value, unit, MeasurementScope::Server, SOURCE, key)
            })
            .collect(),
    };
    ReplicationObservation {
        source: SOURCE.into(),
        fact: ReplicationFact::Native(Box::new(NativeReplicationSample::new(measurements).unwrap())),
    }
}

fn sample(row: &ReplicationObservation) -> &NativeReplicationSample {
    let ReplicationFact::Native(sample) = &row.fact else { panic!("native channel") };
    sample
}

fn sample_mut(row: &mut ReplicationObservation) -> &mut NativeReplicationSample {
    let ReplicationFact::Native(sample) = &mut row.fact else { panic!("native channel") };
    sample
}

fn report(row: &ReplicationObservation) -> ReplicationEntry {
    explain(row, sample(row)).unwrap()
}

fn field<'a>(report: &'a ReplicationEntry, key: &str) -> &'a ReplicationField {
    report
        .fields
        .iter()
        .chain(report.groups.iter().flat_map(|group| &group.fields))
        .find(|field| field.key == key)
        .unwrap()
}

#[test]
fn conditions_require_the_explicit_native_thread_state_and_never_use_historical_errors_or_lag_as_a_cause() {
    use ReplicationCondition::{ApplyingStopped, Connecting, ReceiverStopped, ThreadsStopped};
    for (io, sql, expected) in [
        (Some("No"), Some("No"), vec![ThreadsStopped]),
        (Some("No"), Some("Yes"), vec![ReceiverStopped]),
        (Some("Yes"), Some("No"), vec![ApplyingStopped]),
        (Some("Connecting"), Some("Yes"), vec![Connecting]),
        (Some("Connecting"), Some("No"), vec![Connecting, ApplyingStopped]),
        (Some("Yes"), Some("Yes"), vec![]),
        (None, None, vec![]),
        (Some("Unknown"), None, vec![]),
        (Some("No"), None, vec![ReceiverStopped]),
        (None, Some("No"), vec![ApplyingStopped]),
        (Some("No "), Some("unknown"), vec![]),
    ] {
        let raw = row(io, sql, &[("io-error-code", 2003), ("sql-error-code", 1062), ("lag-seconds", u64::MAX)]);
        assert_eq!(conditions(&raw, sample(&raw)), Some(expected.clone()));
        let explanation = report(&raw);
        assert_eq!(explanation.conditions, expected, "{io:?}/{sql:?}");
        assert_eq!(field(&explanation, "io-error-code").value.as_deref(), Some("2003"));
        assert_eq!(field(&explanation, "sql-error-code").value.as_deref(), Some("1062"));
    }
}

#[test]
fn exact_coordinates_stay_with_their_own_files_and_native_clocks_keep_their_original_spelling() {
    culprit_core::i18n::scope("en", || {
        let mut raw = row(
            Some("Yes"),
            Some("Yes"),
            &[
                ("received-source-log-position", 9_007_199_254_740_993),
                ("executed-source-log-position", u64::MAX),
                ("relay-log-position", 4),
                ("source-port", 3306),
            ],
        );
        sample_mut(&mut raw).measurements.properties.extend([
            ("source_log_file".into(), "bin.000002".into()),
            ("relay_source_log_file".into(), "bin.000001".into()),
            ("relay_log_file".into(), "relay.000042".into()),
            ("io_error_at_without_offset".into(), "261115 01:02:03".into()),
            ("source_last_event_at_without_offset".into(), "2026-11-15 00:59:58".into()),
        ]);
        let before = serde_json::to_value(&raw).unwrap();
        let explanation = report(&raw);
        assert_eq!(field(&explanation, "received-source-log-position").value.as_deref(), Some("9007199254740993"));
        assert_eq!(field(&explanation, "executed-source-log-position").value.as_deref(), Some("18446744073709551615"));
        assert_eq!(field(&explanation, "source_log_file").value.as_deref(), Some("bin.000002"));
        assert_eq!(field(&explanation, "relay_source_log_file").value.as_deref(), Some("bin.000001"));
        assert_eq!(field(&explanation, "relay_log_file").value.as_deref(), Some("relay.000042"));
        assert_eq!(field(&explanation, "source-port").value.as_deref(), Some("3306"));
        assert_eq!(field(&explanation, "io_error_at_without_offset").value.as_deref(), Some("261115 01:02:03"));
        assert_eq!(
            field(&explanation, "source_last_event_at_without_offset").value.as_deref(),
            Some("2026-11-15 00:59:58")
        );
        assert_eq!(serde_json::to_value(&raw).unwrap(), before);
        assert!(explanation.conditions.is_empty());
        assert!(
            explanation
                .fields
                .iter()
                .chain(explanation.groups.iter().flat_map(|group| &group.fields))
                .all(|field| !field.key.contains("backlog") && !field.key.contains("eta"))
        );
    });
}

#[test]
fn zero_unknown_disabled_and_withheld_are_distinct_without_inventing_missing_configuration() {
    let mut raw = row(None, None, &[("configured-delay", 0), ("auto-position", 0)]);
    sample_mut(&mut raw).measurements.properties.extend([
        ("channel".into(), "".into()),
        ("parallel_mode".into(), "none".into()),
        ("replicate_ignore_db".into(), "".into()),
        ("withheld_fields".into(), "replicate_wild_do_table".into()),
    ]);
    let explanation = report(&raw);
    assert!(explanation.conditions.is_empty());
    assert_eq!(field(&explanation, "configured-delay").value.as_deref(), Some("0"));
    assert_eq!(field(&explanation, "remaining-delay").value, None);
    assert_eq!(field(&explanation, "lag-seconds").value, None);
    assert_eq!(field(&explanation, "auto-position").value.as_deref(), Some("0"));
    assert_eq!(field(&explanation, "parallel_mode").value.as_deref(), Some("none"));
    assert_eq!(field(&explanation, "replicate_ignore_db").value.as_deref(), Some(""));
    assert_eq!(field(&explanation, "replicate_do_db").value, None);
    assert_eq!(field(&explanation, "replicate_wild_do_table").value, None);
    assert_eq!(field(&explanation, "withheld_fields").value.as_deref(), Some("replicate_wild_do_table"));
}

#[test]
fn native_parallel_totals_keep_large_counts_and_do_not_authorize_rates_or_worker_counts() {
    culprit_core::i18n::scope("en", || {
        let mut raw =
            row(Some("Yes"), Some("Yes"), &[("ddl-groups", 9_007_199_254_740_993), ("retried-transactions", 0)]);
        let native = sample_mut(&mut raw);
        native.measurements.properties.insert("parallel_mode".into(), "optimistic".into());
        for counter in &mut native.measurements.counters {
            counter.nature = CounterNature::Cumulative;
            counter.reset = ResetMarker::Unknown;
        }
        let explanation = report(&raw);
        assert_eq!(field(&explanation, "ddl-groups").value.as_deref(), Some("9007199254740993"));
        assert_eq!(field(&explanation, "retried-transactions").value.as_deref(), Some("0"));
        assert_eq!(field(&explanation, "parallel_mode").value.as_deref(), Some("optimistic"));
        assert!(
            explanation
                .groups
                .iter()
                .flat_map(|group| &group.fields)
                .all(|field| !field.key.contains("worker-count") && !field.key.contains("per-second"))
        );
    });
}

#[test]
fn privacy_and_reference_projection_never_copy_error_sql_gtid_sets_or_claim_an_application_session() {
    let mut raw = row(Some("No"), Some("No"), &[("io-error-code", 1045)]);
    sample_mut(&mut raw).measurements.properties.extend([
        ("last_io_error".into(), "PRIVATE_ERROR_SQL_CANARY".into()),
        ("source_user".into(), "PRIVATE_LOGIN_CANARY".into()),
        ("source_password".into(), "PRIVATE_PASSWORD_CANARY".into()),
        ("retrieved_gtid_set".into(), "PRIVATE_GTID_SET_CANARY".into()),
    ]);
    sample_mut(&mut raw).session = Some(NativeActivityIdentity { id: "42".into(), generation: "107".into() });
    let explanation = report(&raw);
    assert_eq!(explanation.reference.source, raw.source);
    assert_eq!(explanation.reference.observation_key, raw.fact.key());
    assert_eq!(explanation.scope, MeasurementScope::Server);
    assert!(explanation.session.is_none());
    assert!(explanation.unresolved_session_id.is_none());
    let json = serde_json::to_string(&explanation).unwrap();
    for canary in
        ["PRIVATE_ERROR_SQL_CANARY", "PRIVATE_LOGIN_CANARY", "PRIVATE_PASSWORD_CANARY", "PRIVATE_GTID_SET_CANARY"]
    {
        assert!(!json.contains(canary), "{canary}");
    }
    assert!(!json.contains("measurements"));
}

#[test]
fn another_native_namespace_scope_or_body_cannot_borrow_the_channels_explanation() {
    let mut raw = row(Some("Yes"), Some("Yes"), &[]);
    raw.source = "other-native-source".into();
    assert!(explain(&raw, sample(&raw)).is_none());
    raw.source = SOURCE.into();
    let mut substituted = sample(&raw).clone();
    substituted.measurements.properties.insert("io_running".into(), "No".into());
    assert!(explain(&raw, &substituted).is_none());
    let native = sample_mut(&mut raw);
    native.measurements.scope = MeasurementScope::Database;
    assert!(explain(&raw, sample(&raw)).is_none());
}
