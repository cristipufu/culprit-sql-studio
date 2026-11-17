use super::*;
use culprit_core::{ErrorCode, MeasurementScope, SourceState, telemetry::*};
use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

const SOURCE: &str = "replication-channels";

fn channel(id: usize, stopped: bool) -> ReplicationObservation {
    let measurements = NativeCounterSet {
        ratios: Vec::new(),
        ratio_gaps: Vec::new(),
        identity: NativeIdentity::new(format!("channel-{id}")),
        scope: MeasurementScope::Server,
        counters: vec![NativeCounterSample::gauge(
            "received-source-log-position",
            9_007_199_254_740_993,
            CounterUnit::Bytes,
            MeasurementScope::Server,
            SOURCE,
            "read_source_log_pos",
        )],
        properties: BTreeMap::from([
            ("channel".into(), format!("channel-{id}")),
            ("io_running".into(), if stopped { "No" } else { "Yes" }.into()),
            ("sql_running".into(), "Yes".into()),
            ("source_log_file".into(), "bin.000007".into()),
            ("io_error_at_without_offset".into(), "261115 01:02:03".into()),
            ("last_sql_error".into(), "private-error-sql-password-canary".into()),
        ]),
    };
    ReplicationObservation {
        source: SOURCE.into(),
        fact: ReplicationFact::Native(Box::new(NativeReplicationSample::new(measurements).unwrap())),
    }
}

fn data(rows: Vec<ReplicationObservation>) -> Box<Report> {
    let mut data: Box<Report> =
        serde_json::from_str(include_str!("../../../../fixtures/mysql-demo.report.json")).unwrap();
    data.snapshot.pressure.as_mut().unwrap().replication = Some(Box::new(ReplicationSnapshot {
        context: ObservationReadContext::collector(
            SystemTime::UNIX_EPOCH,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        )
        .unwrap(),
        sources: vec![StateSourceCoverage::observed(SOURCE.into(), rows.len() as u32, false)],
        observations: rows,
    }));
    data
}

fn native(data: &mut Report) -> &mut ReplicationSnapshot {
    data.snapshot.pressure.as_mut().unwrap().replication.as_deref_mut().unwrap()
}

#[test]
fn native_replication_is_bounded_after_all_states_are_examined_and_keeps_exact_references_and_values() {
    let mut rows = (0..REPLICATION_ENTRIES + 5).map(|index| channel(index, false)).collect::<Vec<_>>();
    let stopped = channel(REPLICATION_ENTRIES + 5, true);
    let stopped_key = stopped.fact.key();
    rows.push(stopped);
    let mut data = data(rows);
    let mut stale = culprit_engine::explain_replication(native(&mut data), true, 256).unwrap();
    stale.title = "stale-derived-report-canary".into();
    data.replication = Some(Box::new(stale));
    let before = serde_json::to_value(&data.snapshot.pressure.as_ref().unwrap().replication).unwrap();
    let value = report(&data);
    let evidence = &value["replication_evidence"];
    assert_eq!(evidence["native_rows"], REPLICATION_ENTRIES + 6);
    assert_eq!(evidence["observed_rows"], REPLICATION_ENTRIES + 6);
    assert_eq!(evidence["state_rows"], 1);
    assert_eq!(evidence["presented_rows"], REPLICATION_ENTRIES);
    assert_eq!(evidence["entries"].as_array().unwrap().len(), REPLICATION_ENTRIES);
    assert_eq!(evidence["entries_limited"], true);
    assert_eq!(evidence["sources"][0]["state"], "partial");
    assert_eq!(evidence["read_complete"], true);
    assert_eq!(evidence["read_context"]["native"], false);
    assert_eq!(evidence["entries"][0]["reference"]["observation_key"], stopped_key);
    assert_eq!(evidence["entries"][0]["reference"]["source"], SOURCE);
    assert_eq!(evidence["entries"][0]["scope"], "server");
    assert_eq!(evidence["entries"][0]["conditions"][0], "receiver-stopped");
    let body = serde_json::to_string(evidence).unwrap();
    assert!(body.contains(&format::digits("9007199254740993".into())));
    assert!(body.contains("bin.000007") && body.contains("261115 01:02:03"));
    assert!(!body.contains("stale-derived-report-canary") && !body.contains("private-error-sql-password-canary"));
    assert_eq!(before, serde_json::to_value(&data.snapshot.pressure.as_ref().unwrap().replication).unwrap());
    assert!(value["replication"].is_array());
}

#[test]
fn empty_partial_denied_and_unsupported_replication_reads_preserve_source_flags_without_native_points() {
    for coverage in [
        StateSourceCoverage::collected(SOURCE.into(), 0),
        StateSourceCoverage::observed(SOURCE.into(), 0, false),
        StateSourceCoverage::unavailable(SOURCE.into(), ErrorCode::PermissionDenied, None),
        StateSourceCoverage::not_applicable(SOURCE.into()),
    ] {
        let expected_state = coverage.state;
        let mut data = data(vec![]);
        native(&mut data).sources = vec![coverage];
        let value = report(&data);
        let evidence = &value["replication_evidence"];
        assert_eq!(evidence["sources"][0]["state"], serde_json::to_value(expected_state).unwrap());
        assert_eq!(evidence["native_rows"], 0);
        assert_eq!(evidence["observed_rows"], 0);
        assert_eq!(evidence["state_rows"], 0);
        assert_eq!(evidence["presented_rows"], 0);
        assert_eq!(evidence["entries"].as_array().unwrap().len(), 0);
        assert_eq!(evidence["entries_limited"], false);
        if expected_state == SourceState::Unavailable {
            assert!(evidence["sources"][0]["reason"].as_str().is_some_and(|reason| !reason.is_empty()));
        }
        assert!(value.get("replication_evidence_unavailable").is_none());
    }
}

#[test]
fn native_read_bounds_do_not_serialize_private_database_routes_or_invent_qualified_member_coverage() {
    let mut data = data(vec![channel(0, false)]);
    native(&mut data).context = ObservationReadContext::Native {
        context: Box::new(NativeReadContext {
            target: culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Mysql,
                "app",
                "private-replication-route-canary.invalid",
                3306,
                None,
            )
            .into(),
            partition: SourcePartition::Target,
            observed_from: "2026-11-16T10:00:00Z".into(),
            observed_until: "2026-11-16T10:00:01Z".into(),
            database_writable: None,
        }),
    };
    let value = report(&data);
    let evidence = &value["replication_evidence"];
    assert_eq!(evidence["read_context"]["native"], true);
    assert_eq!(evidence["read_context"]["observed_from"], "2026-11-16T10:00:00Z");
    assert_eq!(evidence["sources"][0]["state"], "partial");
    assert!(
        evidence["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == &json!(culprit_core::t!("view.replication.limit.origin")))
    );
    assert!(!serde_json::to_string(evidence).unwrap().contains("private-replication-route-canary"));
}

#[test]
fn contradictory_replication_membership_is_unavailable_instead_of_a_clean_empty_report() {
    let mut data = data(vec![channel(0, false)]);
    native(&mut data).sources[0].records = 0;
    let value = report(&data);
    assert!(value.get("replication_evidence").is_none());
    assert_eq!(value["replication_evidence_unavailable"], culprit_core::t!("mcp.waits.replication_unavailable"));
    assert!(!serde_json::to_string(&value).unwrap().contains("private-error-sql-password-canary"));
    data.snapshot.pressure.as_mut().unwrap().replication = None;
    let absent = report(&data);
    assert!(absent.get("replication_evidence").is_none() && absent.get("replication_evidence_unavailable").is_none());
}
