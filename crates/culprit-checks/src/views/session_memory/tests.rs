use super::*;
use culprit_core::{SourceState, telemetry::*};

fn observation(id: &str) -> SessionMemoryObservation {
    SessionMemoryObservation {
        source: "session memory".into(),
        identity: NativeIdentity::new(id)
            .qualified("server_uuid", "native-server")
            .qualified("server_started_second", "1577836800"),
        observed_at: "2020-01-01T12:00:00.250Z".into(),
        session: NativeActivityIdentity { id: "61".into(), generation: id.into() },
        tracking_enabled: true,
        current_bytes: Some("9007199254740993".into()),
        controlled_current_bytes: Some("0".into()),
        high_water_bytes: Some("18446744073709551615".into()),
        controlled_high_water_bytes: None,
        current_context: Some(SessionMemoryContext {
            request: NativeActivityIdentity { id: "9".into(), generation: "statement-start-2".into() },
            observed_at: "2020-01-01T12:00:00.750Z".into(),
            database: Some("context-only-other-db".into()),
            query_hash: Some("context-only-digest".into()),
        }),
        native_properties: [("INSTRUMENTED".into(), "YES".into())].into(),
    }
}

fn coverage(state: SourceState) -> SourceOutcome {
    SourceOutcome { source: "session memory".into(), state, reason: None, reason_message: None }
}

#[test]
fn exact_tracked_bytes_and_session_peaks_remain_distinct_without_query_ownership_or_float_rounding() {
    let row = observation("native-thread-1");
    let report = explain(&[&row], Some(&coverage(SourceState::Collected))).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.observation_identity, row.identity);
    assert_eq!(entry.observation_key, row.key().unwrap());
    assert!(entry.title.contains("9007199254740993"));
    assert!(entry.explanation.contains("18446744073709551615"));
    assert!(entry.explanation.contains(&t!("view.session_memory.metric.controlled_current", n = "0")));
    assert!(entry.explanation.contains(&t!("view.session_memory.context.observed")));
    assert!(report.limitations.contains(&t!("view.session_memory.limit.context")));
    assert!(!serde_json::to_string(&report).unwrap().contains("context-only-"));
    assert!(!entry.investigation.is_empty() && !entry.verify.is_empty());
}

#[test]
fn disabled_tracking_and_missing_measurements_do_not_become_zero_allocations() {
    let mut row = observation("native-thread-1");
    row.tracking_enabled = false;
    row.current_bytes = None;
    row.controlled_current_bytes = None;
    row.high_water_bytes = None;
    row.controlled_high_water_bytes = None;
    let report = explain(&[&row], Some(&coverage(SourceState::Partial))).unwrap();
    assert_eq!(report.entries[0].title, t!("view.session_memory.disabled.title"));
    assert!(report.limitations.contains(&t!("view.session_memory.limit.disabled")));
    assert!(!report.entries[0].explanation.contains(&t!("view.session_memory.metric.current", n = "0")));
    row.current_bytes = Some("0".into());
    assert!(explain(&[&row], None).is_err(), "disabled tracking cannot carry a measured zero");
    row.current_bytes = None;
    row.tracking_enabled = true;
    let report = explain(&[&row], None).unwrap();
    assert_eq!(report.entries[0].title, t!("view.session_memory.unknown.title"));
}

#[test]
fn a_historical_high_water_and_absent_current_event_never_become_current_use_or_an_allocator() {
    let mut row = observation("native-thread-1");
    row.current_bytes = Some("0".into());
    row.current_context = None;
    let report = explain(&[&row], None).unwrap();
    assert_eq!(report.entries[0].title, t!("view.session_memory.tracked.title", bytes = "0"));
    assert!(report.entries[0].explanation.contains(&t!("view.session_memory.context.unavailable")));
    assert!(report.limitations.contains(&t!("view.session_memory.limit.high_water")));
    assert!(!report.entries[0].title.contains("18446744073709551615"));
}

#[test]
fn empty_unavailable_and_partial_sources_preserve_qualification_and_native_identity_order() {
    for state in [SourceState::Empty, SourceState::Partial, SourceState::Unavailable, SourceState::NotApplicable] {
        let report = explain(&[], Some(&coverage(state))).unwrap();
        assert_eq!(report.source_state, Some(state));
        assert!(report.entries.is_empty());
        assert_eq!(report.summary, t!("view.session_memory.summary.empty"));
    }
    let first = observation("first");
    let second = observation("second");
    assert_eq!(explain(&[&first, &second], None).unwrap(), explain(&[&second, &first], None).unwrap());
    let report = explain(&[&first], None).unwrap();
    assert!(report.limitations.contains(&t!("view.memory_participants.limit.source_unknown")));
}

#[test]
fn conflicting_session_sources_duplicate_lifetimes_and_invalid_values_are_rejected() {
    let row = observation("native-thread-1");
    let mut changed = row.clone();
    changed.current_bytes = Some("1".into());
    assert!(explain(&[&row, &changed], None).is_err());
    changed.identity.id = "another-thread".into();
    changed.source = "another source".into();
    assert!(explain(&[&row, &changed], None).is_err());
    changed.source = row.source.clone();
    changed.current_bytes = Some("-1".into());
    assert!(explain(&[&changed], None).is_err());
    assert!(explain(&[&row], Some(&coverage(SourceState::Unavailable))).is_err());
    assert!(explain(&[], Some(&coverage(SourceState::Collected))).is_err());
}
