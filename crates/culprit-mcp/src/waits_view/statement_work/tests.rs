use super::*;
use culprit_core::{
    DatabaseIdentity, Engine, ErrorCode, MeasurementScope, Metric, SourceState, StatementWorkReport, telemetry::*,
};

const SOURCE: &str = "private-native-counter-source-canary";
const DATABASE: &str = "public-logical-database";

fn data(count: usize) -> Box<Report> {
    let mut data: Box<Report> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mysql-demo.report.json"
    )))
    .unwrap();
    data.snapshot.id = "private-snapshot-id-canary".into();
    data.snapshot.server.database = DATABASE.into();
    let context = NativeReadContext {
        target: DatabaseIdentity::server(Engine::Mysql, DATABASE, "private-native-route-canary.invalid", 3306, None)
            .into(),
        partition: SourcePartition::Target,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        database_writable: None,
    };
    data.snapshot.bind_identity(context.target.database().unwrap().clone());
    let observations = (0..count)
        .map(|index| {
            let sample = StatementCounterSample {
                member: None,
                identity: NativeQueryIdentity {
                    query_id: format!("{index:032x}"),
                    qualifiers: [("schema".into(), DATABASE.into())].into(),
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
            OperationalObservation {
                source: SOURCE.into(),
                key: sample.key().unwrap(),
                fact: OperationalFact::StatementCounters(Box::new(sample)),
            }
        })
        .collect::<Vec<_>>();
    let native = StatementCounterSnapshot {
        source: SOURCE.into(),
        context: Some(Box::new(context)),
        coverage: StateSourceCoverage::observed(SOURCE.into(), count as u32, false),
        observations,
    };
    assert!(native.is_valid());
    install(&mut data, native);
    data
}

// Report fixtures exercise transport projection. Native interpretation is covered by the shared view tests.
fn install(data: &mut Report, native: StatementCounterSnapshot) {
    let entries = native
        .observations
        .iter()
        .map(|row| {
            let OperationalFact::StatementCounters(point) = &row.fact else { panic!() };
            let outcomes = point.outcomes().unwrap();
            let outcome_metrics = [
                ("view.statement_work.outcome.errors", &outcomes.errors),
                ("view.statement_work.outcome.warnings", &outcomes.warnings),
            ]
            .into_iter()
            .filter_map(|(key, value)| value.as_ref().map(|value| Metric::said_text(key, value)))
            .collect();
            StatementWorkEntry {
                source: row.source.clone(),
                observation_key: row.key.clone(),
                identity: point.identity.clone(),
                scope: point.scope,
                observed_at: point.observed_at.clone(),
                reset: point.reset.clone(),
                work: vec![],
                outcomes,
                outcome_metrics,
            }
        })
        .collect();
    data.statement_work = Some(Box::new(StatementWorkReport {
        source: native.source.clone(),
        context: native.context.clone(),
        source_state: Some(native.coverage.state),
        source_reason: None,
        title: culprit_core::t!("view.statement_work.title"),
        summary: "Recorded counter points.".into(),
        limitations: vec![culprit_core::t!("view.statement_work.limit.reset")],
        entries,
    }));
    data.snapshot.operations.statement_work = Some(Box::new(native));
}

fn compact(data: &Report) -> Value {
    let mut response = json!({});
    attach(&mut response, data);
    response
}

#[test]
fn compact_output_preserves_exact_counts_bounds_and_original_population_while_bounding_entries() {
    let data = data(ENTRY_LIMIT + 3);
    let before = serde_json::to_value(data.snapshot.operations.statement_work.as_ref()).unwrap();
    let value = super::super::report(&data);
    let evidence = &value["statement_work"];
    assert_eq!(evidence["database"], DATABASE);
    assert_eq!(evidence["source_state"], "partial");
    assert_eq!(evidence["complete_scope"], false);
    assert_eq!(evidence["native_rows"], ENTRY_LIMIT + 3);
    assert_eq!(evidence["observed_rows"], ENTRY_LIMIT + 3);
    assert_eq!(evidence["entries_retained"], ENTRY_LIMIT + 3);
    assert_eq!(evidence["entries_presented"], ENTRY_LIMIT);
    assert_eq!(evidence["entries_limited"], true);
    assert_eq!(evidence["entries"].as_array().unwrap().len(), ENTRY_LIMIT);
    assert_eq!(evidence["entries"][0]["outcomes"]["errors"], "9007199254740993");
    assert_eq!(evidence["entries"][0]["outcomes"]["warnings"], "0");
    assert_eq!(evidence["entries"][0]["scope"], "database");
    assert_eq!(evidence["entries"][0]["reset"]["kind"], "unknown");
    assert_eq!(evidence["read_bounds"]["observed_from"], "2020-01-01T12:00:00Z");
    assert_eq!(evidence["read_bounds"]["observed_until"], "2020-01-01T12:00:01Z");
    assert_eq!(before, serde_json::to_value(data.snapshot.operations.statement_work.as_ref()).unwrap());
    assert!(value["operations"].get("statement_work").is_none());
    assert!(!serde_json::to_string(&value["operations"]).unwrap().contains(SOURCE));
    let body = serde_json::to_string(evidence).unwrap();
    for private in [SOURCE, "private-snapshot-id-canary", "private-native-route-canary"] {
        assert!(!body.contains(private));
    }
    assert!(body.len() < 12_000);
}

#[test]
fn opaque_point_references_remain_distinct_for_schema_and_capture_without_exposing_native_ids() {
    let other = data(2);
    let other_points = compact(&other);
    assert_ne!(
        other_points["statement_work"]["entries"][0]["reference"],
        other_points["statement_work"]["entries"][1]["reference"]
    );
    let mut data = data(1);
    let original = compact(&data);
    let left = &original["statement_work"]["entries"][0]["reference"];
    assert_eq!(left.as_str().unwrap().len(), 64);
    assert_eq!(compact(&data)["statement_work"]["entries"][0]["reference"], *left);
    data.snapshot.id = "another-capture".into();
    assert_ne!(compact(&data)["statement_work"]["entries"][0]["reference"], *left);
    data.snapshot.id = "private-snapshot-id-canary".into();
    let mut native = *data.snapshot.operations.statement_work.take().unwrap();
    let OperationalFact::StatementCounters(point) = &mut native.observations[0].fact else { panic!() };
    point.identity.qualifiers.insert("schema".into(), "another_schema".into());
    native.observations[0].key = point.key().unwrap();
    let CollectionTarget::Database(database) = &mut native.context.as_mut().unwrap().target else { panic!() };
    database.database = "another_schema".into();
    data.snapshot.server.database = database.database.clone();
    data.snapshot.bind_identity(database.clone());
    install(&mut data, native);
    assert_ne!(compact(&data)["statement_work"]["entries"][0]["reference"], *left);
}

#[test]
fn empty_partial_denied_and_unsupported_points_keep_unknowns_and_original_source_flags() {
    for coverage in [
        StateSourceCoverage::collected(SOURCE.into(), 0),
        StateSourceCoverage::observed(SOURCE.into(), 0, false),
        StateSourceCoverage::unavailable(SOURCE.into(), ErrorCode::PermissionDenied, None),
        StateSourceCoverage::not_applicable(SOURCE.into()),
    ] {
        let mut data = data(0);
        let mut native = *data.snapshot.operations.statement_work.take().unwrap();
        native.coverage = coverage;
        if matches!(native.coverage.state, SourceState::Unavailable | SourceState::NotApplicable) {
            native.context = None;
        }
        assert!(native.is_valid());
        let state = native.coverage.state;
        install(&mut data, native);
        let value = compact(&data);
        let evidence = &value["statement_work"];
        assert_eq!(evidence["source_state"], serde_json::to_value(state).unwrap());
        assert_eq!(evidence["native_rows"], 0);
        assert_eq!(evidence["entries"], json!([]));
        assert_eq!(evidence["entries_limited"], false);
        if state == SourceState::Unavailable {
            assert_eq!(evidence["source_error_code"], "permission_denied");
        }
    }
    let mut data = data(1);
    let mut native = *data.snapshot.operations.statement_work.take().unwrap();
    let OperationalFact::StatementCounters(point) = &mut native.observations[0].fact else { panic!() };
    point.totals.retain(|counter| counter.key != RuntimeMetricKey::Warnings);
    install(&mut data, native);
    assert_eq!(compact(&data)["statement_work"]["entries"][0]["outcomes"]["warnings"], Value::Null);
}

#[test]
fn a_withheld_or_mismatched_view_is_explicitly_unavailable_and_never_dumps_native_points() {
    for change in ["withheld", "source", "clock"] {
        let mut data = data(1);
        match change {
            "withheld" => data.statement_work = None,
            "source" => data.statement_work.as_mut().unwrap().source = "unrelated-native-source".into(),
            _ => {
                data.statement_work.as_mut().unwrap().context.as_mut().unwrap().observed_until =
                    "2020-01-01T12:00:02Z".into()
            }
        }
        let value = super::super::report(&data);
        assert!(value.get("statement_work").is_none());
        assert_eq!(value["statement_work_unavailable"], culprit_core::t!("view.statement_work.invalid"));
        assert!(value["operations"].get("statement_work").is_none());
        assert!(!serde_json::to_string(&value).unwrap().contains(SOURCE));
    }
    let mut data = data(1);
    data.snapshot.operations.statement_work = None;
    let value = compact(&data);
    assert!(value.get("statement_work").is_none() && value.get("statement_work_unavailable").is_none());
}

#[test]
fn local_member_epoch_and_uninterpreted_native_qualifiers_never_leak_into_compact_evidence() {
    let mut data = data(1);
    let mut native = *data.snapshot.operations.statement_work.take().unwrap();
    let member = culprit_core::engines::ysql_node_member("0123456789abcdef0123456789abcdef").unwrap();
    let context = native.context.as_mut().unwrap();
    context.target =
        DatabaseIdentity::server(Engine::Yugabyte, DATABASE, "private-native-route-canary.invalid", 5433, None).into();
    context.partition = SourcePartition::InstanceLocal {
        member: Some(member.clone()),
        epoch: Some("private-native-epoch-canary".into()),
    };
    data.snapshot.server.engine = Engine::Yugabyte;
    data.snapshot.bind_identity(context.target.database().unwrap().clone());
    let OperationalFact::StatementCounters(point) = &mut native.observations[0].fact else { panic!() };
    point.member = Some(member);
    point.scope = MeasurementScope::Node;
    point.identity = NativeQueryIdentity {
        query_id: "-42".into(),
        qualifiers: [
            ("node".into(), "0123456789abcdef0123456789abcdef".into()),
            ("database_oid".into(), "16384".into()),
            ("userid".into(), "10".into()),
            ("toplevel".into(), "true".into()),
            // Opaque metadata is not an SQL-body channel in this compact projection.
            ("private-context".into(), "private-sql-body-canary".into()),
        ]
        .into(),
    };
    point.totals = vec![StatementCounter {
        key: RuntimeMetricKey::Executions,
        unit: RuntimeMetricUnit::Count,
        total: MeasurementValue::Integer("17".into()),
    }];
    native.observations[0].key = point.key().unwrap();
    assert!(native.is_valid());
    install(&mut data, native);
    let value = compact(&data);
    assert_eq!(value["statement_work"]["database"], DATABASE);
    assert_eq!(value["statement_work"]["entries"][0]["scope"], "node");
    assert_eq!(value["statement_work"]["entries"][0]["reset"]["kind"], "unknown");
    let body = serde_json::to_string(&value).unwrap();
    for private in [
        SOURCE,
        "private-native-route-canary",
        "private-native-epoch-canary",
        "0123456789abcdef0123456789abcdef",
        "private-sql-body-canary",
    ] {
        assert!(!body.contains(private), "{private}");
    }
    // A source that independently declares a reset generation gets an opaque relationship reference.
    // The node fixture above retains its actual unknown counter generation.
    let generation = reset(&ResetMarker::Generation("private-reset-generation-canary".into()));
    assert_eq!(generation["kind"], "generation");
    assert_eq!(generation["reference"].as_str().unwrap().len(), 64);
    assert!(!serde_json::to_string(&generation).unwrap().contains("private-reset-generation-canary"));
}
