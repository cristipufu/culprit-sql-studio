use super::*;
use culprit_core::{DatabaseIdentity, Engine, ErrorCode, MeasurementScope, ResetMarker, SourceState, telemetry::*};

fn snapshot() -> Box<Snapshot> {
    let mut snapshot: Box<Snapshot> =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mysql-demo.json")))
            .unwrap();
    let database = DatabaseIdentity::server(Engine::Mysql, &snapshot.server.database, "fixture.invalid", 3306, None);
    snapshot.bind_identity(database.clone());
    let sample = StatementCounterSample {
        member: None,
        identity: NativeQueryIdentity {
            query_id: "0123456789abcdef0123456789abcdef".into(),
            qualifiers: [("schema".into(), snapshot.server.database.clone())].into(),
        },
        scope: MeasurementScope::Database,
        observed_at: "2020-01-01T12:00:00.123456Z".into(),
        reset: ResetMarker::Unknown,
        totals: vec![
            StatementCounter {
                key: RuntimeMetricKey::Errors,
                unit: RuntimeMetricUnit::Count,
                total: MeasurementValue::Integer("9007199254740993".into()),
            },
            StatementCounter {
                key: RuntimeMetricKey::Warnings,
                unit: RuntimeMetricUnit::Count,
                total: MeasurementValue::Integer("0".into()),
            },
        ],
        distributions: vec![],
    };
    let source = "native-counter-source";
    snapshot.operations.statement_work = Some(Box::new(StatementCounterSnapshot {
        source: source.into(),
        context: Some(Box::new(NativeReadContext {
            target: database.into(),
            partition: SourcePartition::Target,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            database_writable: None,
        })),
        coverage: StateSourceCoverage::observed(source.into(), 1, false),
        observations: vec![OperationalObservation {
            source: source.into(),
            key: sample.key().unwrap(),
            fact: OperationalFact::StatementCounters(Box::new(sample)),
        }],
    }));
    snapshot
}

#[test]
fn the_scan_report_reuses_the_exact_shared_counter_view_without_adding_an_error_diagnosis() {
    let snapshot = snapshot();
    let expected = crate::statement_work::explain(snapshot.operations.statement_work.as_ref().unwrap(), true).unwrap();
    let baseline_findings = {
        let mut baseline = snapshot.clone();
        baseline.operations.statement_work = None;
        crate::analyze(*baseline).findings.into_iter().map(|finding| finding.id).collect::<Vec<_>>()
    };
    let report = crate::analyze(*snapshot);
    assert_eq!(
        serde_json::to_value(report.statement_work.as_ref().unwrap()).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(report.findings.into_iter().map(|finding| finding.id).collect::<Vec<_>>(), baseline_findings);
    assert_eq!(report.statement_work.unwrap().entries[0].outcomes.warnings.as_deref(), Some("0"));
}

#[test]
fn another_engine_database_or_endpoint_cannot_supply_this_scans_counter_report() {
    for change in ["engine", "database", "endpoint", "incarnation"] {
        let mut snapshot = snapshot();
        let database_name = snapshot.server.database.clone();
        let context = snapshot.operations.statement_work.as_mut().unwrap().context.as_mut().unwrap();
        let CollectionTarget::Database(database) = &mut context.target else { panic!() };
        match change {
            "engine" => database.engine = Engine::Postgres,
            "database" => database.database = "another_database".into(),
            "endpoint" => {
                *database = DatabaseIdentity::server(Engine::Mysql, &database_name, "another.invalid", 3306, None)
            }
            _ => database.incarnation = Some("another-incarnation".into()),
        }
        let notes = snapshot.notes.len();
        assert!(super::explain(&mut snapshot).is_none(), "{change}");
        assert_eq!(snapshot.notes.len(), notes + 1);
    }
}

#[test]
fn unavailable_native_evidence_remains_unavailable_without_borrowing_metadata_or_zero_counts() {
    let mut snapshot = snapshot();
    snapshot.operations.statement_work = Some(Box::new(StatementCounterSnapshot::unavailable(
        "native-counter-source",
        ErrorCode::PermissionDenied,
        None,
    )));
    let report = super::explain(&mut snapshot).unwrap();
    assert_eq!(report.source_state, Some(SourceState::Unavailable));
    assert!(report.context.is_none() && report.entries.is_empty());
    snapshot.operations.statement_work = None;
    assert!(super::explain(&mut snapshot).is_none());
}
