use super::*;

fn captured() -> CleanupSnapshot {
    CleanupSnapshot {
        source: "native cleanup".into(),
        observed_from: "2026-11-13T10:00:00Z".into(),
        observed_until: "2026-11-13T10:00:01Z".into(),
        observations: vec![OperationalObservation {
            source: "native cleanup".into(),
            key: "oldest-cleanup-transaction".into(),
            fact: OperationalFact::MssqlCleanupHorizon(MssqlCleanupHorizon {
                oldest_confirmed: None,
                oldest_unattributed_versioning: None,
            }),
        }],
    }
}

#[test]
fn each_native_cleanup_source_roundtrips_its_own_read_bounds_and_original_key() {
    let value = captured();
    assert!(value.is_valid());
    let restored: CleanupSnapshot = serde_json::from_value(serde_json::to_value(&value).unwrap()).unwrap();
    assert_eq!(restored, value);
    let mut other = value.clone();
    other.observed_from = "2026-11-13T10:00:02Z".into();
    other.observed_until = "2026-11-13T10:00:03Z".into();
    assert!(other.is_valid(), "another source's later read does not authorize an atomic join");
    assert_eq!(other.observations, value.observations);
}

#[test]
fn cleanup_snapshots_reject_foreign_sources_duplicates_and_invalid_read_clocks() {
    let original = captured();
    let mut wrong = original.clone();
    wrong.observations[0].source = "different source".into();
    assert!(wrong.observations[0].is_valid());
    assert!(!wrong.is_valid());
    wrong = original.clone();
    wrong.observations.push(wrong.observations[0].clone());
    assert!(!wrong.is_valid());
    wrong = original.clone();
    wrong.observed_until = "2026-11-13T09:59:59Z".into();
    assert!(!wrong.is_valid());
    wrong = original;
    wrong.observed_from = "2026-11-13T10:00:00".into();
    assert!(!wrong.is_valid());
}

#[test]
fn bounded_empty_cleanup_points_do_not_authorize_unrelated_operational_facts() {
    let mut captured = captured();
    captured.observations = vec![captured.observations[0].clone(); MAX_CLEANUP_OBSERVATIONS + 1];
    assert!(!captured.is_valid());
    captured.observations.clear();
    assert!(captured.is_valid(), "source outcomes still determine empty, failed or unavailable coverage");
    let sample = NativeCounterSample::gauge(
        "bytes",
        0,
        CounterUnit::Bytes,
        crate::MeasurementScope::Database,
        "unrelated",
        "bytes",
    );
    captured.observations.push(OperationalObservation {
        source: captured.source.clone(),
        key: sample.key.clone(),
        fact: OperationalFact::Counter(sample),
    });
    assert!(captured.observations[0].is_valid());
    assert!(!captured.is_valid(), "an arbitrary native counter does not establish a cleanup mechanism");
}
