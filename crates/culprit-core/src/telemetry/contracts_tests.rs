use super::*;

fn reference(id: &str) -> NativeDefinitionReference {
    NativeDefinitionReference { native: NativeIdentity::new(id), definition_id: Some(digest(id.as_bytes())) }
}
fn runtime() -> RuntimeMeasurement {
    RuntimeMeasurement {
        key: NativeMeasurementKey {
            native: NativeIdentity::new("native-row"),
            references: StatementReferences { query: Some(reference("query")), plan: None },
        },
        executions: "9007199254740993".into(),
        metrics: vec![RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Milliseconds, 1.25)],
        first_execution_at: None,
        last_execution_at: None,
    }
}
#[test]
fn planless_measurements_preserve_unknown_metrics_and_native_units() {
    let mut row = runtime();
    assert!(row.is_valid() && row.key.references.is_resolved());
    assert!(row.metric(&RuntimeMetricKey::CpuTime).is_none());
    row.metrics.push(RuntimeMetric::average(
        RuntimeMetricKey::LogicalReads,
        RuntimeMetricUnit::Pages { bytes_per_page: 16384 },
        7.0,
    ));
    assert!(row.is_valid());
    let encoded = serde_json::to_value(&row).unwrap();
    assert_eq!(encoded["executions"], "9007199254740993");
    assert!(encoded["key"]["references"]["plan"].is_null());
    assert_eq!(encoded["metrics"][1]["unit"]["bytes_per_page"], 16384);
    assert_eq!(serde_json::from_value::<RuntimeMeasurement>(encoded).unwrap(), row);
}
#[test]
fn invalid_or_ambiguous_metrics_cannot_enter_an_export() {
    let row = runtime();
    for unit in [RuntimeMetricUnit::Count, RuntimeMetricUnit::Pages { bytes_per_page: 0 }] {
        let mut changed = row.clone();
        changed.metrics[0].unit = unit;
        assert!(!changed.is_valid());
    }
    for value in [f64::NAN, f64::INFINITY, -1.0] {
        let mut changed = row.clone();
        changed.metrics[0].average = value;
        assert!(!changed.is_valid());
    }
    let mut duplicate = row.clone();
    duplicate.metrics.push(row.metrics[0].clone());
    assert!(!duplicate.is_valid());
    let mut resolved_plan_without_query = row;
    resolved_plan_without_query.key.references = StatementReferences { query: None, plan: Some(reference("plan")) };
    assert!(!resolved_plan_without_query.is_valid());
}
#[test]
fn native_identity_is_canonical_but_its_qualifiers_are_not_interpreted() {
    let a = NativeIdentity::new("opaque").qualified("userid", "17").qualified("dbid", "42");
    let b = NativeIdentity::new("opaque").qualified("dbid", "42").qualified("userid", "17");
    assert_eq!(serde_json::to_vec(&a).unwrap(), serde_json::to_vec(&b).unwrap());
    assert!(a.is_valid());
    assert!(!a.qualified("dbid", "").is_valid());
    assert!(!NativeIdentity::new("id").qualified("bad\nname", "value").is_valid());
}
#[test]
fn source_dimensions_are_explicit_and_do_not_require_replica_groups() {
    let mut history = QueryHistoryDescription {
        interval: TelemetryInterval {
            native_id: "interval".into(),
            from: "2020-01-01T00:00:00Z".into(),
            until: "2020-01-01T01:00:00Z".into(),
        },
        dimensions: Default::default(),
        native_properties: Default::default(),
    };
    assert!(history.is_valid());
    history.dimensions.insert("node".into(), vec!["primary".into(), "secondary".into()]);
    let source = CollectionDescription {
        target: crate::DatabaseIdentity::server(crate::Engine::Postgres, "sample", "fixture.invalid", 5432, None)
            .into(),
        source: "fixture".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::QueryHistory(history.clone()),
    };
    let mut key = runtime().key;
    assert!(!source.accepts_measurement(&key));
    key.native.qualifiers.insert("node".into(), "primary".into());
    assert!(source.accepts_measurement(&key));
    key.native.qualifiers.insert("node".into(), "invented".into());
    assert!(!source.accepts_measurement(&key));
    history.dimensions.insert("node".into(), vec!["primary".into(), "primary".into()]);
    assert!(!history.is_valid());
}
#[test]
fn query_only_records_get_stable_native_ids_but_unresolved_queries_do_not() {
    let source = CollectionDescription {
        target: crate::DatabaseIdentity::server(crate::Engine::Postgres, "sample", "fixture.invalid", 5432, None)
            .into(),
        source: "fixture".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::QueryHistory(QueryHistoryDescription {
            interval: TelemetryInterval {
                native_id: "1".into(),
                from: "2020-01-01T00:00:00Z".into(),
                until: "2020-01-01T01:00:00Z".into(),
            },
            dimensions: Default::default(),
            native_properties: Default::default(),
        }),
    };
    let capture = CaptureIdentity { observation_id: "capture".into(), revision: None };
    let row = runtime();
    let first = ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(row.clone())).unwrap();
    assert!(first.record_id.is_some());
    let mut correction = row.clone();
    correction.executions = "19".into();
    correction.key.references.query.as_mut().unwrap().definition_id = Some(digest(b"another-definition"));
    let second = ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(correction)).unwrap();
    assert_eq!(
        first.record_id, second.record_id,
        "definition resolution and corrections must not create a second native row"
    );
    let mut unresolved = row;
    unresolved.key.references.query.as_mut().unwrap().definition_id = None;
    assert!(
        ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(unresolved)).unwrap().record_id.is_none()
    );
}
#[test]
fn cumulative_statement_counters_preserve_exact_values_and_reset_uncertainty() {
    let sample = StatementCounterSample {
        scope: crate::MeasurementScope::Database,
        distributions: vec![],
        identity: NativeQueryIdentity {
            query_id: "fingerprint".into(),
            qualifiers: [("userid".into(), "17".into())].into(),
        },
        observed_at: "2020-01-01T00:00:00Z".into(),
        reset: crate::ResetMarker::Unknown,
        totals: vec![
            StatementCounter {
                key: RuntimeMetricKey::Executions,
                unit: RuntimeMetricUnit::Count,
                total: MeasurementValue::Integer("9007199254740993".into()),
            },
            StatementCounter {
                key: RuntimeMetricKey::Duration,
                unit: RuntimeMetricUnit::Milliseconds,
                total: MeasurementValue::Real(1.25),
            },
        ],
    };
    assert!(sample.is_valid());
    let encoded = serde_json::to_value(&sample).unwrap();
    assert_eq!(encoded["totals"][0]["total"]["value"], "9007199254740993");
    assert_eq!(serde_json::from_value::<StatementCounterSample>(encoded).unwrap(), sample);
    let mut reset = sample.clone();
    reset.reset = crate::ResetMarker::Generation("new-epoch".into());
    assert!(reset.is_valid());
    assert_eq!(sample.key().unwrap(), reset.key().unwrap(), "reset evidence changes the sample, not its native query");
    reset.totals[0].total = MeasurementValue::Real(1.5);
    assert!(!reset.is_valid(), "execution counts must stay exact integers");
}

#[test]
fn native_picosecond_totals_keep_exact_precision_and_node_scope() {
    let sample = StatementCounterSample {
        identity: NativeQueryIdentity { query_id: "digest".into(), qualifiers: Default::default() },
        scope: crate::MeasurementScope::Node,
        observed_at: "2026-11-11T00:00:00Z".into(),
        reset: crate::ResetMarker::Unknown,
        totals: vec![StatementCounter {
            key: RuntimeMetricKey::Duration,
            unit: RuntimeMetricUnit::Picoseconds,
            total: MeasurementValue::Integer(u64::MAX.to_string()),
        }],
        distributions: vec![],
    };
    assert!(sample.is_valid());
    let encoded = serde_json::to_value(&sample).unwrap();
    assert_eq!(encoded["scope"], "node");
    assert_eq!(encoded["totals"][0]["unit"]["kind"], "picoseconds");
    assert_eq!(encoded["totals"][0]["total"]["value"], u64::MAX.to_string());
    assert_eq!(serde_json::from_value::<StatementCounterSample>(encoded).unwrap(), sample);
}

#[test]
fn sql_markers_in_literals_never_change_statement_provenance() {
    for engine in [crate::Engine::Postgres, crate::Engine::Mysql, crate::Engine::Yugabyte] {
        assert_eq!(
            StatementOrigin::from_sql(Some("SELECT '/* culprit:monitor */'"), engine).0,
            StatementOrigin::Application
        );
        assert_eq!(StatementOrigin::from_sql(Some("/* culprit */ SELECT 1"), engine).0, StatementOrigin::Culprit);
        let sql = format!("/* culprit:monitor */ /* culprit:template:{} */ SELECT 1", digest(b"template"));
        assert_eq!(
            StatementOrigin::from_sql(Some(&sql), engine),
            (StatementOrigin::CulpritMonitoring, Some(digest(b"template")))
        );
        assert_eq!(StatementOrigin::from_sql(None, engine).0, StatementOrigin::Unknown);
    }
}

#[test]
fn native_aggregate_precision_and_fractional_wait_units_are_preserved() {
    // Providers may expose average and extrema at different native precision.
    let metric = RuntimeMetric {
        minimum: Some(1.0),
        maximum: Some(1.0),
        ..RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Microseconds, 1.25)
    };
    assert!(metric.is_valid());
    let mut wait = WaitMeasurement {
        key: runtime().key,
        category: "native-io".into(),
        unit: RuntimeMetricUnit::Seconds,
        total: MeasurementValue::Real(0.000125),
        average: None,
        minimum: None,
        maximum: None,
    };
    assert!(wait.is_valid());
    let encoded = serde_json::to_value(&wait).unwrap();
    assert_eq!(encoded["total"]["value"], 0.000125);
    wait.unit = RuntimeMetricUnit::Count;
    assert!(!wait.is_valid());
}
