use super::*;
use crate::Engine;

const SOURCE: &str = crate::source_id::SESSION_MEMORY;
const INSTANCE_UUID: &str = "b4571c74-3001-4000-8000-000000000001";

fn observation() -> SessionMemoryObservation {
    SessionMemoryObservation {
        source: SOURCE.into(),
        identity: NativeIdentity::new("120")
            .qualified("server_uuid", INSTANCE_UUID)
            .qualified("server_started_second", "1770000000"),
        observed_at: "2026-11-13T10:00:00.500000Z".into(),
        session: NativeActivityIdentity { id: "75".into(), generation: "120".into() },
        tracking_enabled: true,
        current_bytes: Some(u64::MAX.to_string()),
        controlled_current_bytes: Some("0".into()),
        high_water_bytes: Some(u64::MAX.to_string()),
        controlled_high_water_bytes: None,
        current_context: Some(SessionMemoryContext {
            request: NativeActivityIdentity { id: "5".into(), generation: "999".into() },
            observed_at: "2026-11-13T10:00:00.600000Z".into(),
            database: Some("app".into()),
            query_hash: Some("0123456789abcdef0123456789abcdef".into()),
        }),
        native_properties: [("INSTRUMENTED".into(), "YES".into())].into(),
    }
}

fn snapshot() -> SessionMemorySnapshot {
    SessionMemorySnapshot {
        source: SOURCE.into(),
        observed_from: "2026-11-13T10:00:00.100000Z".into(),
        observed_until: "2026-11-13T10:00:00.900000Z".into(),
        resource: SharedResourceIdentity {
            engine: Engine::Mysql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("mysql-server").qualified("server_uuid", INSTANCE_UUID),
            native: NativeIdentity::new(INSTANCE_UUID),
        },
        observations: vec![observation()],
    }
}

#[test]
fn exact_native_allocations_zero_and_unknown_survive_the_operational_wire_contract() {
    let value = observation();
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: SOURCE.into(),
        key: value.key().unwrap(),
        fact: OperationalFact::SessionMemory(Box::new(value.clone())),
    });
    assert!(record.is_valid());
    let encoded = serde_json::to_string(&record).unwrap();
    let decoded: TelemetryRecord = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, record);
    let OperationalFact::SessionMemory(roundtrip) = record_fact(&decoded) else { panic!("session memory fact") };
    assert_eq!(roundtrip.current_bytes.as_deref(), Some("18446744073709551615"));
    assert_eq!(roundtrip.controlled_current_bytes.as_deref(), Some("0"));
    assert!(roundtrip.controlled_high_water_bytes.is_none());
    assert_eq!(roundtrip.session, value.session);
    assert!(decoded.definition_payloads().next().is_none(), "a native digest is context, never SQL text");
}

fn record_fact(record: &TelemetryRecord) -> &OperationalFact {
    let TelemetryRecord::Operational(observation) = record else { panic!("operational observation") };
    &observation.fact
}

#[test]
fn disabled_tracking_preserves_unknown_values_and_rejects_a_fabricated_measured_zero() {
    let mut value = observation();
    value.tracking_enabled = false;
    value.current_bytes = None;
    value.controlled_current_bytes = None;
    value.high_water_bytes = None;
    value.controlled_high_water_bytes = None;
    assert!(value.is_valid());
    let mut current = value.clone();
    current.current_bytes = Some("0".into());
    let mut controlled = value.clone();
    controlled.controlled_current_bytes = Some("0".into());
    let mut high_water = value.clone();
    high_water.high_water_bytes = Some("0".into());
    let mut controlled_high_water = value;
    controlled_high_water.controlled_high_water_bytes = Some("0".into());
    assert!([current, controlled, high_water, controlled_high_water].iter().all(|value| !value.is_valid()));
}

#[test]
fn allocation_values_are_unsigned_native_integers_without_overflow_or_float_substitution() {
    for invalid in ["-1", "18446744073709551616", "NaN", "1.5", "1e6", ""] {
        let mut value = observation();
        value.current_bytes = Some(invalid.into());
        assert!(!value.is_valid(), "{invalid}");
    }
}

#[test]
fn native_session_lifetime_identity_is_independent_of_counters_and_current_statement_context() {
    let mut value = observation();
    let original = value.key().unwrap();
    value.current_bytes = Some("128".into());
    value.high_water_bytes = Some("512".into());
    value.observed_at = "2026-11-13T10:00:00.700000Z".into();
    value.current_context.as_mut().unwrap().request.generation = "another-request".into();
    assert!(value.is_valid());
    assert_eq!(value.key().unwrap(), original);
    value.identity = NativeIdentity::new("121")
        .qualified("server_uuid", INSTANCE_UUID)
        .qualified("server_started_second", "1770000000");
    value.session.generation = "121".into();
    assert_ne!(value.key().unwrap(), original);
}

#[test]
fn independently_observed_context_and_allocations_must_each_fit_the_authenticated_capture_bounds() {
    let value = snapshot();
    assert!(value.is_valid());
    assert_ne!(value.observations[0].observed_at, value.observations[0].current_context.as_ref().unwrap().observed_at);
    for clock in [
        "2026-11-13T10:00:00.099999Z",
        "2026-11-13T10:00:00.900001Z",
        "2026-11-13T10:00:00.500000+01:00",
        "2026-11-13T10:00:00",
        "not-a-native-clock",
    ] {
        let mut outside = value.clone();
        outside.observations[0].observed_at = clock.into();
        assert!(!outside.is_valid(), "allocation clock: {clock}");
        let mut outside = value.clone();
        outside.observations[0].current_context.as_mut().unwrap().observed_at = clock.into();
        assert!(!outside.is_valid(), "context clock: {clock}");
    }
    let mut independently_ordered = value;
    independently_ordered.observations[0].current_context.as_mut().unwrap().observed_at =
        "2026-11-13T10:00:00.400000Z".into();
    assert!(independently_ordered.is_valid(), "the common contract does not invent atomicity or observation order");
}

#[test]
fn a_session_capture_rejects_pool_ownership_source_mismatch_and_duplicate_native_entities() {
    let mut value = snapshot();
    value.resource.kind = SharedResourceKind::Pool;
    assert!(!value.is_valid());
    let mut value = snapshot();
    value.observations[0].source = "another native source".into();
    assert!(!value.is_valid());
    let mut value = snapshot();
    let mut changed_counters = value.observations[0].clone();
    changed_counters.current_bytes = Some("1".into());
    value.observations.push(changed_counters);
    assert!(!value.is_valid(), "one mutable session cannot acquire two identities from different counter values");
    let mut value = snapshot();
    value.observed_until = "2026-11-13T10:00:00Z".into();
    assert!(!value.is_valid());
}

#[test]
fn an_operational_wrapper_cannot_substitute_another_source_for_the_native_session_fact() {
    let value = observation();
    let row = OperationalObservation {
        source: "different source".into(),
        key: value.key().unwrap(),
        fact: OperationalFact::SessionMemory(Box::new(value)),
    };
    assert!(!row.is_valid());
}

#[test]
fn sharing_session_allocations_sanitizes_physical_identity_in_both_resource_and_observation_references() {
    let mut original: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mysql-demo.json")).unwrap();
    let evidence = snapshot();
    original.operations.session_memory = Some(evidence.clone());
    let shared = crate::redact::redact_snapshot(&original);
    let serialized = serde_json::to_string(&shared).unwrap();
    assert!(!serialized.contains(INSTANCE_UUID), "a row qualifier must not republish the sanitized hosting UUID");
    let shared_evidence = shared.operations.session_memory.as_ref().unwrap();
    assert!(shared_evidence.is_valid());
    assert_eq!(
        shared_evidence.observations[0].identity.qualifiers.get("server_uuid"),
        shared_evidence.resource.namespace.qualifiers.get("server_uuid"),
        "the same physical identity must keep its equality relationships"
    );
    assert_eq!(shared_evidence.observations[0].current_bytes, evidence.observations[0].current_bytes);
    assert_eq!(shared_evidence.observations[0].current_context, evidence.observations[0].current_context);
    assert_eq!(original.operations.session_memory.as_ref().unwrap(), &evidence);
    assert_eq!(
        serde_json::to_value(&shared).unwrap(),
        serde_json::to_value(crate::redact::redact_snapshot(&shared)).unwrap()
    );
}
