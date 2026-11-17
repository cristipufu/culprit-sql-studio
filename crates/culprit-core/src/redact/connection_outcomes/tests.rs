use super::*;
use crate::{ConnectionOutcomeMetricKind, ConnectionOutcomeObservation, DatabaseIdentity, Engine, MeasurementScope};
use std::collections::BTreeMap;

fn sample() -> Box<ConnectionOutcomeSnapshot> {
    let row = ConnectionOutcomeObservation {
        source: "pg_stat_database".into(),
        counters: NativeCounterSet {
            ratios: Vec::new(),
            ratio_gaps: Vec::new(),
            identity: NativeIdentity::new("pg_stat_database")
                .qualified("dbid", "private-oid")
                .qualified("member", "private-physical-member"),
            scope: MeasurementScope::Database,
            properties: BTreeMap::from([
                ("datname".into(), "fixture".into()),
                ("server_version_num".into(), "180000".into()),
                ("counter_observed_at".into(), "2020-01-02T00:00:00Z".into()),
                ("private_metadata".into(), "postgres://user:secret@private-native-host".into()),
            ]),
            counters: vec![NativeCounterSample {
                key: "sessions".into(),
                value: MeasurementValue::Integer("9007199254740993".into()),
                unit: CounterUnit::Count,
                nature: CounterNature::Cumulative,
                scope: MeasurementScope::Database,
                reset: ResetMarker::At("2020-01-01T00:00:00Z".into()),
                native_object: "pg_stat_database".into(),
                native_name: "sessions".into(),
                native_instance: None,
                native_type: None,
            }],
        },
    };
    Box::new(ConnectionOutcomeSnapshot {
        target: DatabaseIdentity::server(
            Engine::Postgres,
            "fixture",
            "private-native-host",
            5432,
            Some("private-native-incarnation".into()),
        )
        .into(),
        context: ObservationReadContext::Collector {
            observed_from: "2026-01-01T00:00:00Z".into(),
            observed_until: "2026-01-01T00:00:01Z".into(),
        },
        sources: vec![StateSourceCoverage::observed("pg_stat_database".into(), 1, true)],
        observations: vec![row],
    })
}

#[test]
fn sharing_preserves_native_semantics_and_rates_without_physical_identity_or_private_metadata() {
    let read = sample();
    let before = read.rate(ConnectionOutcomeMetricKind::SessionsEstablished);
    let redacted = redact(read).unwrap();
    assert_eq!(redacted.rate(ConnectionOutcomeMetricKind::SessionsEstablished), before);
    let json = serde_json::to_string(&redacted).unwrap();
    for private in
        ["private-oid", "private-physical-member", "private-native-host", "private-native-incarnation", "user:secret"]
    {
        assert!(!json.contains(private), "{private}");
    }
    assert_eq!(serde_json::to_string(&redact(redacted).unwrap()).unwrap(), json);
}

#[test]
fn invalid_original_identity_is_withheld_with_a_stable_visible_gap_when_sharing() {
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut read = sample();
    read.observations[0].counters.identity.id.clear();
    assert!(!read.is_valid_read(false));
    snapshot.pressure.get_or_insert_default().connection_outcomes = Some(read);
    let shared = crate::redact::redact_snapshot(&snapshot);
    assert!(shared.pressure.as_ref().unwrap().connection_outcomes.is_none());
    let source = shared.sources.iter().find(|source| source.source == crate::source_id::SESSIONS).unwrap();
    assert_eq!(source.state, crate::SourceState::Unavailable);
    assert_eq!(source.reason_message, Some(crate::message!("view.connection_outcomes.invalid")));
    assert!(shared.notes.iter().any(|note| note.collector == crate::source_id::SESSIONS));
    let json = serde_json::to_string(&shared).unwrap();
    assert!(!json.contains("private-native-host"));
    assert!(!json.contains("user:secret"));
    assert_eq!(
        serde_json::to_value(crate::redact::redact_snapshot(&shared)).unwrap(),
        serde_json::to_value(shared).unwrap()
    );
}

#[test]
fn a_forged_public_issue_with_private_arguments_is_still_masked() {
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.record_unavailable_evidence(
        crate::source_id::SESSIONS,
        crate::i18n::Message::catalog(
            crate::key!("view.connection_outcomes.invalid"),
            &[("private", crate::i18n::MessageValue::Text("private-argument-canary".into()))],
        ),
    );
    let shared = crate::redact::redact_snapshot(&snapshot);
    assert!(!serde_json::to_string(&shared).unwrap().contains("private-argument-canary"));
    assert!(
        shared
            .sources
            .iter()
            .find(|source| source.source == crate::source_id::SESSIONS)
            .unwrap()
            .reason_message
            .is_none()
    );
}

fn unmapped_row(source: &str, identity: &str) -> ConnectionOutcomeObservation {
    let mut row = sample().observations.remove(0);
    row.source = source.into();
    row.counters.identity = NativeIdentity::new(identity).qualified("private-qualifier-key", "private-qualifier-value");
    row.counters.properties = BTreeMap::from([(
        "/private/property-key".into(),
        "postgres://private-login:private-password@private-unmapped-host".into(),
    )]);
    let counter = &mut row.counters.counters[0];
    counter.key = "/private/counter-key".into();
    counter.native_object = "/private/native-object".into();
    counter.native_name = "postgres://private-login:private-password@private-counter-name".into();
    counter.native_instance = Some("private-counter-instance".into());
    counter.native_type = Some("private-counter-type".into());
    counter.reset = ResetMarker::Generation("private-reset-generation".into());
    row
}

fn add_row(read: &mut ConnectionOutcomeSnapshot, row: ConnectionOutcomeObservation) {
    if let Some(source) = read.sources.iter_mut().find(|source| source.source == row.source) {
        source.records += 1;
    } else {
        read.sources.push(StateSourceCoverage::observed(row.source.clone(), 1, false));
    }
    read.observations.push(row);
}

#[test]
fn mapped_and_unmapped_labels_are_masked_without_changing_exact_measurements_or_qualified_rates() {
    let mut read = sample();
    let mapped = &mut read.observations[0];
    mapped.counters.identity.qualifiers.insert("/private/qualifier-key".into(), "private-qualifier-value".into());
    mapped.counters.properties.insert("has_sessions".into(), "true".into());
    mapped.counters.properties.insert("/private/property-key".into(), "private-property-value".into());
    let known = &mut mapped.counters.counters[0];
    known.native_object = "postgres://private-login:private-password@private-object-host".into();
    known.native_name = "/private/native-name".into();
    let mut active = known.clone();
    let mut extra = known.clone();
    active.key = "active_time".into();
    active.native_object = mapped.source.clone();
    active.native_name = "active_time".into();
    active.value = MeasurementValue::Real(123456.125);
    active.unit = CounterUnit::Milliseconds;
    extra.key = "/private/extra-counter-key".into();
    extra.value = MeasurementValue::Integer("18446744073709551615".into());
    mapped.counters.counters.extend([active, extra]);
    add_row(&mut read, unmapped_row("private-native-source/path", "private-native-identity"));
    assert!(read.is_valid());
    let original = serde_json::to_value(&read).unwrap();
    let rate = read.rate(ConnectionOutcomeMetricKind::SessionsEstablished).unwrap();
    let measurements = |read: &ConnectionOutcomeSnapshot| {
        read.observations
            .iter()
            .map(|row| {
                row.counters
                    .counters
                    .iter()
                    .map(|counter| (counter.value.clone(), counter.unit, counter.nature, counter.scope))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let redacted = redact(read.clone()).unwrap();
    assert!(redacted.is_valid());
    assert_eq!(measurements(&redacted), measurements(&read));
    assert_eq!(redacted.rate(ConnectionOutcomeMetricKind::SessionsEstablished), Some(rate));
    assert_eq!(redacted.sources.iter().map(|source| source.records).collect::<Vec<_>>(), vec![1, 1]);
    let mapped = &redacted.observations[0];
    assert_eq!(mapped.source, "pg_stat_database");
    assert_eq!(mapped.counters.identity.id, "pg_stat_database");
    assert_eq!(mapped.counters.counters[0].key, "sessions");
    let active = &mapped.counters.counters[1];
    assert_eq!(
        (active.key.as_str(), active.native_name.as_str(), active.native_object.as_str()),
        ("active_time", "active_time", "pg_stat_database")
    );
    for key in ["datname", "server_version_num", "counter_observed_at", "has_sessions"] {
        assert_eq!(mapped.counters.properties.get(key), read.observations[0].counters.properties.get(key));
    }
    let unmapped = &redacted.observations[1];
    assert_eq!(unmapped.source, opaque("private-native-source/path"));
    assert_eq!(redacted.sources[1].source, unmapped.source);
    assert!(redacted.profile(unmapped).is_none());
    let json = serde_json::to_string(&redacted).unwrap();
    assert!(!json.contains("private"));
    assert!(!json.contains("user:secret"));
    assert!(json.contains("9007199254740993") && json.contains("18446744073709551615"));
    assert_eq!(
        serde_json::to_value(redact(redacted).unwrap()).unwrap(),
        serde_json::from_str::<serde_json::Value>(&json).unwrap()
    );
    assert_eq!(serde_json::to_value(read).unwrap(), original);
}

#[test]
fn an_unmapped_row_does_not_hide_a_source_positively_recognized_in_another_row() {
    let mut read = sample();
    add_row(&mut read, unmapped_row("pg_stat_database", "private-unrecognized-schema"));
    assert!(read.is_valid());
    let redacted = redact(read).unwrap();
    assert_eq!(redacted.sources[0].source, "pg_stat_database");
    assert_eq!(redacted.sources[0].records, 2);
    assert!(redacted.observations.iter().all(|row| row.source == "pg_stat_database"));
    assert!(redacted.profile(&redacted.observations[0]).is_some());
    assert!(redacted.profile(&redacted.observations[1]).is_none());
    assert!(!serde_json::to_string(&redacted).unwrap().contains("private"));
}

fn colliding_read(kind: &str) -> Box<ConnectionOutcomeSnapshot> {
    let mut read = sample();
    let private = "private-collision-label";
    let masked = opaque(private);
    match kind {
        "property" => {
            let properties = &mut read.observations[0].counters.properties;
            properties.insert(private.into(), "value-one".into());
            properties.insert(masked, "value-two".into());
        }
        "qualifier" => {
            let qualifiers = &mut read.observations[0].counters.identity.qualifiers;
            qualifiers.insert(private.into(), "value-one".into());
            qualifiers.insert(masked, "value-two".into());
        }
        "counter" => {
            let mut first = read.observations[0].counters.counters[0].clone();
            first.key = private.into();
            let mut second = first.clone();
            second.key = masked;
            read.observations[0].counters.counters.extend([first, second]);
        }
        "source" => {
            add_row(&mut read, unmapped_row(private, "native-one"));
            add_row(&mut read, unmapped_row(&masked, "native-two"));
        }
        "identity" => {
            add_row(&mut read, unmapped_row("pg_stat_database", private));
            add_row(&mut read, unmapped_row("pg_stat_database", &masked));
        }
        _ => panic!("unknown controlled collision"),
    }
    assert!(read.is_valid(), "the original {kind} labels are distinct");
    read
}

#[test]
fn masking_collisions_withhold_the_carrier_without_overwriting_evidence_and_keep_an_explicit_gap() {
    for kind in ["property", "qualifier", "counter", "source", "identity"] {
        let read = colliding_read(kind);
        let original = serde_json::to_value(&read).unwrap();
        assert_eq!(
            redact(read.clone()).unwrap_err().detail,
            Some(crate::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)),
            "{kind}"
        );
        let mut snapshot: crate::Snapshot =
            serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
        snapshot.pressure.get_or_insert_default().connection_outcomes = Some(read);
        let shared = crate::redact::redact_snapshot(&snapshot);
        assert!(shared.pressure.as_ref().unwrap().connection_outcomes.is_none(), "{kind}");
        let source = shared.sources.iter().find(|source| source.source == crate::source_id::SESSIONS).unwrap();
        assert_eq!(source.state, crate::SourceState::Unavailable);
        assert_eq!(source.reason_message, Some(crate::message!("view.connection_outcomes.invalid")));
        let json = serde_json::to_string(&shared).unwrap();
        assert!(!json.contains("private"), "{kind}");
        assert!(!json.contains("user:secret"), "{kind}");
        assert_eq!(
            serde_json::to_value(crate::redact::redact_snapshot(&shared)).unwrap(),
            serde_json::to_value(shared).unwrap()
        );
        assert_eq!(serde_json::to_value(&snapshot.pressure.as_ref().unwrap().connection_outcomes).unwrap(), original);
    }
}
