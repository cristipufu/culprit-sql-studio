use super::*;
use culprit_core::MeasurementScope;
use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

fn slot(name: &str, status: &str) -> ReplicationObservation {
    let set = NativeCounterSet {
        ratios: Vec::new(),
        ratio_gaps: Vec::new(),
        identity: NativeIdentity::new(name),
        scope: MeasurementScope::Server,
        counters: vec![NativeCounterSample::gauge(
            "kept",
            9_007_199_254_740_993,
            CounterUnit::Bytes,
            MeasurementScope::Server,
            "pg_replication_slots",
            "kept",
        )],
        properties: BTreeMap::from([
            ("kind".into(), "slot".into()),
            ("slot_name".into(), name.into()),
            ("slot_type".into(), "logical".into()),
            ("wal_status".into(), status.into()),
        ]),
    };
    ReplicationObservation {
        source: "pg_replication_slots".into(),
        fact: ReplicationFact::Native(Box::new(NativeReplicationSample::new(set).unwrap())),
    }
}

fn snapshot(rows: Vec<ReplicationObservation>) -> ReplicationSnapshot {
    ReplicationSnapshot {
        mssql_scope: None,
        context: ObservationReadContext::collector(
            SystemTime::UNIX_EPOCH,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        )
        .unwrap(),
        sources: vec![StateSourceCoverage::observed("pg_replication_slots".into(), rows.len() as u32, false)],
        observations: rows,
    }
}

#[test]
fn the_same_original_point_supplies_one_exact_reference_and_does_not_gain_a_sql_owner() {
    let input = snapshot(vec![slot("consumer-a", "lost")]);
    let before = serde_json::to_value(&input).unwrap();
    let report = explain(&input, true).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.state_rows, 1);
    assert_eq!(report.entries[0].conditions, [ReplicationCondition::SlotInvalid]);
    assert_eq!(report.entries[0].reference.observation_key, input.observations[0].fact.key());
    assert_eq!(report.entries[0].scope, MeasurementScope::Server);
    assert!(serde_json::to_string(&report).unwrap().contains("9007199254740993"));
    assert!(report.entries[0].session.is_none() && report.entries[0].unresolved_session_id.is_none());
    assert_eq!(serde_json::to_value(&input).unwrap(), before);
}

#[test]
fn an_archive_gap_cannot_rewrite_native_membership_counts_or_permission_failure_as_empty() {
    let mut input = snapshot(vec![slot("consumer-a", "lost")]);
    input.observations.clear();
    assert!(explain(&input, true).is_err());
    let report = explain(&input, false).unwrap();
    assert_eq!((report.native_rows, report.observed_rows, report.presented_rows), (1, 0, 0));
    assert_eq!(report.sources[0].state, SourceState::Partial);
    assert!(report.entries.is_empty() && !report.read_complete);
    input.sources[0] = StateSourceCoverage::unavailable(
        "pg_replication_slots".into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    );
    let unavailable = explain(&input, true).unwrap();
    assert_eq!(unavailable.sources[0].state, SourceState::Unavailable);
    assert!(unavailable.entries.is_empty());
}

#[test]
fn presentation_is_bounded_after_all_native_states_are_examined_and_does_not_hide_the_late_invalid_slot() {
    let mut rows = (0..MAX_REPLICATION_REPORT_ENTRIES + 30)
        .map(|i| slot(&format!("consumer-{i}"), "reserved"))
        .collect::<Vec<_>>();
    let fault = slot("last-invalid-consumer", "lost");
    let key = fault.fact.key();
    rows.push(fault);
    let input = snapshot(rows);
    let report = explain(&input, true).unwrap();
    assert!(report.entries_limited);
    assert_eq!(report.native_rows, input.observations.len() as u32);
    assert_eq!(report.observed_rows, report.native_rows);
    assert_eq!(report.presented_rows, MAX_REPLICATION_REPORT_ENTRIES as u32);
    assert_eq!(report.state_rows, 1);
    assert_eq!(report.entries[0].reference.observation_key, key);
    assert_eq!(input.observations.len(), MAX_REPLICATION_REPORT_ENTRIES + 31);
}

#[test]
fn an_unknown_native_family_does_not_fall_through_to_another_engines_replication_rules() {
    let mut input = snapshot(vec![slot("consumer-a", "lost")]);
    input.sources[0].source = "future-replication-source".into();
    input.observations[0].source = "future-replication-source".into();
    let report = explain(&input, true).unwrap();
    assert_eq!(report.state_rows, 0);
    assert!(report.entries[0].conditions.is_empty());
    assert!(report.entries[0].session.is_none());
}

#[test]
fn source_and_duplicate_identity_contradictions_fail_before_native_conditions_are_rendered() {
    let mut input = snapshot(vec![slot("consumer-a", "lost")]);
    input.sources[0].records = 2;
    input.observations.push(input.observations[0].clone());
    assert!(explain(&input, true).is_err());
    input.observations.pop();
    input.sources[0].records = 1;
    input.sources[0].state = SourceState::Empty;
    assert!(explain(&input, false).is_err());
}
