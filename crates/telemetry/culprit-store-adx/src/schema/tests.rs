use super::*;
use culprit_core::telemetry::{
    MeasurementValue, NativeDefinitionReference, NativeIdentity, RuntimeMetric, RuntimeMetricKey, RuntimeMetricUnit,
    StatementReferences,
};

#[test]
fn one_schema_drives_column_types_mapping_and_exact_value_verification() {
    let schema = Schema::new(
        "FixtureV1",
        [
            Field::required("Count", "/count", ScalarType::ExactCounter),
            Field::optional("Label", "/label", ScalarType::String),
            Field::required("Facts", "/facts", ScalarType::JsonContainer),
        ],
    )
    .unwrap();
    let values = schema.project(&json!({"count":"9007199254740993","label":null,"facts":{"a":2.0}})).unwrap();
    assert_eq!(values, vec![json!("9007199254740993"), json!(false), json!(""), json!({"a":2.0})]);
    let expected = projection::digest(&values).unwrap();
    let returned = vec![json!("9007199254740993.0"), json!(false), json!(""), json!("{\"a\":2}")];
    assert_eq!(schema.returned_digest(&returned).unwrap(), expected);
    let mut changed = returned.clone();
    changed[0] = json!("9007199254740992");
    assert_ne!(schema.returned_digest(&changed).unwrap(), expected);
    changed = returned.clone();
    changed[1] = json!(true);
    assert_ne!(schema.returned_digest(&changed).unwrap(), expected);
    assert!(schema.create_table().contains("Count:decimal, HasLabel:bool, Label:string, Facts:string"));
    assert!(schema.verification_projection().contains("Count=tostring(Count)"));
    assert!(schema.verification_projection().contains("Facts=tostring(Facts)"));
    assert!(schema.create_mapping().unwrap().contains("$.HasLabel"));
    let wire = schema.encode(&values).unwrap();
    assert_eq!(wire.last(), Some(&b'\n'));
    assert_eq!(serde_json::from_slice::<Value>(&wire).unwrap()["Count"], "9007199254740993");
}
#[test]
fn schema_changes_cannot_reuse_projection_indexes_and_invalid_column_sets_are_refused() {
    let first = Schema::new("FixtureV1", [Field::required("Value", "/value", ScalarType::Long)]).unwrap();
    let second = Schema::new("FixtureV1", [Field::required("Value", "/other", ScalarType::Long)]).unwrap();
    assert_ne!(first.digest(), second.digest());
    assert!(Schema::new("unsafe; command", [Field::required("Value", "/value", ScalarType::Long)]).is_err());
    assert!(
        Schema::new(
            "FixtureV1",
            [
                Field::optional("Value", "/value", ScalarType::String),
                Field::required("HasValue", "/present", ScalarType::Bool)
            ]
        )
        .is_err()
    );
    assert!(first.project(&json!({})).is_err());
    assert!(first.returned_digest(&[]).is_err());
}

#[test]
fn structured_json_is_stored_as_text_without_promoting_native_strings_or_losing_counter_digits() {
    let schema = Schema::new(
        "FixtureV1",
        [
            Field::required("Facts", "/facts", ScalarType::JsonContainer),
            Field::optional("Missing", "/missing", ScalarType::JsonContainer),
        ],
    )
    .unwrap();
    let facts = json!({"at":"2020-01-01T06:00:00Z","id":"ABCDEF01-2345-6789-ABCD-EF0123456789","count":"18446744073709551615","values":["21",null,2.0]});
    let expected = schema.project(&json!({"facts":facts})).unwrap();
    let wire: Value = serde_json::from_slice(&schema.encode(&expected).unwrap()).unwrap();
    assert_eq!(wire["Facts"].as_str().unwrap(), serde_json::to_string(&facts).unwrap());
    assert!(wire["Missing"].is_null());
    let returned = vec![wire["Facts"].clone(), json!("")];
    assert_eq!(schema.returned_digest(&returned).unwrap(), projection::digest(&expected).unwrap());
    let mut changed = facts.clone();
    changed["at"] = json!("2020-01-01T06:00:00.0000000Z");
    assert_ne!(
        schema.returned_digest(&[json!(changed.to_string()), json!("")]).unwrap(),
        projection::digest(&expected).unwrap(),
        "ingestion must not silently rewrite a nested native string"
    );
    assert!(schema.create_table().contains("Facts:string"));
}

#[test]
fn runtime_projection_covers_the_native_contract_and_preserves_counts_units_and_nulls() {
    use culprit_core::telemetry::{NativeMeasurementKey, RuntimeMeasurement};
    let native = RuntimeMeasurement {
        key: NativeMeasurementKey {
            native: NativeIdentity::new("1")
                .qualified("plan_id", "2")
                .qualified("replica_group_id", "3")
                .qualified("execution_type", "4".to_string()),
            references: StatementReferences {
                query: Some(NativeDefinitionReference {
                    native: NativeIdentity::new("q"),
                    definition_id: Some("b".repeat(64)),
                }),
                plan: Some(NativeDefinitionReference {
                    native: NativeIdentity::new("2"),
                    definition_id: Some("a".repeat(64)),
                }),
            },
        },
        executions: "9007199254740993".into(),
        metrics: vec![
            RuntimeMetric {
                minimum: Some(2.5),
                maximum: Some(24.5),
                standard_deviation: Some(3.5),
                ..RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Microseconds, 13.5)
            },
            RuntimeMetric::average(RuntimeMetricKey::CpuTime, RuntimeMetricUnit::Microseconds, 7.5),
            RuntimeMetric::average(
                RuntimeMetricKey::LogicalReads,
                RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
                11.5,
            ),
            RuntimeMetric::average(
                RuntimeMetricKey::PhysicalReads,
                RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
                1.5,
            ),
            RuntimeMetric::average(RuntimeMetricKey::Rows, RuntimeMetricUnit::Count, 17.5),
            RuntimeMetric::average(RuntimeMetricKey::Parallelism, RuntimeMetricUnit::Ratio, 4.5),
            RuntimeMetric::average(
                RuntimeMetricKey::PeakMemory,
                RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
                19.5,
            ),
            RuntimeMetric::average(
                RuntimeMetricKey::Native("mssql.clr-time".into()),
                RuntimeMetricUnit::Microseconds,
                5.5,
            ),
            RuntimeMetric::average(RuntimeMetricKey::LogBytes, RuntimeMetricUnit::Bytes, 23.5),
            RuntimeMetric::average(
                RuntimeMetricKey::TemporaryStorage,
                RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
                29.5,
            ),
        ],
        first_execution_at: Some("2026-11-11T06:00:00Z".into()),
        last_execution_at: Some("2026-11-11T06:01:00Z".into()),
    };
    let schema = Schema::from_logical(culprit_collector::analytical::schema::queries::runtime(&[]).unwrap()).unwrap();
    let input = json!({"record":{"data":native}});
    let projected = schema.project(&input).unwrap();
    let wire: Value = serde_json::from_slice(&schema.encode(&projected).unwrap()).unwrap();
    assert_eq!(wire["Executions"], "9007199254740993");
    let metrics: Value = serde_json::from_str(wire["Metrics"].as_str().unwrap()).unwrap();
    let metric = |kind: &str| metrics.as_array().unwrap().iter().find(|metric| metric["key"]["kind"] == kind);
    assert_eq!(metric("duration").unwrap()["average"], 13.5);
    assert_eq!(metric("cpu-time").unwrap()["unit"]["kind"], "microseconds");
    assert_eq!(metric("logical-reads").unwrap()["unit"]["bytes_per_page"], 8192);
    assert_eq!(metric("log-bytes").unwrap()["average"], 23.5);
    assert!(metric("logical-writes").is_none());
    let mapped = schema.fields.iter().map(|field| field.pointer).collect::<BTreeSet<_>>();
    for name in input["record"]["data"].as_object().unwrap().keys() {
        if name == "key" {
            assert!(mapped.contains("/record/data/key/native/id"));
            assert!(mapped.contains("/record/data/key/native/qualifiers"));
            assert!(mapped.contains("/record/data/key/references"));
        } else {
            assert!(mapped.contains(format!("/record/data/{name}").as_str()), "unmapped native field {name}");
        }
    }
}

#[test]
fn wait_totals_keep_exact_milliseconds_and_are_not_confused_with_average_waits() {
    use culprit_core::telemetry::{NativeMeasurementKey, WaitMeasurement};
    let native = WaitMeasurement {
        key: NativeMeasurementKey {
            native: NativeIdentity::new("1")
                .qualified("plan_id", "2")
                .qualified("replica_group_id", "3")
                .qualified("execution_type", "0".to_string()),
            references: StatementReferences {
                query: Some(NativeDefinitionReference {
                    native: NativeIdentity::new("q"),
                    definition_id: Some("b".repeat(64)),
                }),
                plan: Some(NativeDefinitionReference {
                    native: NativeIdentity::new("2"),
                    definition_id: Some("a".repeat(64)),
                }),
            },
        },
        category: "7".to_string(),
        unit: RuntimeMetricUnit::Milliseconds,
        total: MeasurementValue::Integer("9007199254740993".into()),
        average: Some(13.5),
        minimum: Some(MeasurementValue::Integer("2".into())),
        maximum: Some(MeasurementValue::Integer("23".into())),
    };
    let schema = Schema::from_logical(culprit_collector::analytical::schema::queries::waits(&[]).unwrap()).unwrap();
    let projected = schema.project(&json!({"record":{"data":native}})).unwrap();
    let wire: Value = serde_json::from_slice(&schema.encode(&projected).unwrap()).unwrap();
    assert_eq!(wire["WaitCategory"], "7");
    assert_eq!(
        serde_json::from_str::<Value>(wire["TotalWait"].as_str().unwrap()).unwrap()["value"],
        "9007199254740993"
    );
    assert_eq!(wire["TimeUnit"], "milliseconds");
    assert_eq!(wire["AverageWait"], 13.5);
    assert_eq!(serde_json::from_str::<Value>(wire["MinimumWait"].as_str().unwrap()).unwrap()["value"], "2");
    assert_eq!(serde_json::from_str::<Value>(wire["MaximumWait"].as_str().unwrap()).unwrap()["value"], "23");
}

fn wire(schema: &Schema, data: impl serde::Serialize) -> Value {
    let values = schema.project(&json!({"record":{"data":data}})).unwrap();
    serde_json::from_slice(&schema.encode(&values).unwrap()).unwrap()
}

#[test]
fn definitions_keep_native_provenance_and_payload_references_without_copying_large_sql_into_adx() {
    use culprit_core::telemetry::*;
    let text = "SELECT variable_name; ".repeat(20_000);
    let sql_sha = digest(text.as_bytes());
    let identity = NativeQueryIdentity {
        query_id: "7".into(),
        qualifiers: [
            ("context_settings_id".into(), "3".into()),
            ("object_id".into(), "0".into()),
            ("query_hash".into(), "0123456789ABCDEF".into()),
            ("initial_compile_at".into(), "2026-11-11T06:00:00Z".into()),
        ]
        .into(),
    };
    let query = QueryDefinition {
        id: digest(b"definition"),
        origin: StatementOrigin::CulpritMonitoring,
        monitoring_template_id: Some(digest(b"monitoring-template")),
        identity: identity.clone(),
        context: Default::default(),
        context_available: false,
        source_text_bytes: Some("840000".into()),
        sql: DefinitionPayload::Available { text, sha256: sql_sha.clone(), redaction_version: REDACTION_VERSION },
    };
    assert!(query.is_valid());
    let projected = wire(
        &Schema::from_logical(culprit_collector::analytical::schema::definitions::queries(&[]).unwrap()).unwrap(),
        &query,
    );
    assert_eq!(projected["StatementOrigin"], "culprit-monitoring");
    assert_eq!(projected["SqlPayloadSha256"], sql_sha);
    assert_eq!(projected["NativeSourceTextBytes"], "840000");
    assert_eq!(projected["CompilationContextAvailable"], false);
    assert!(serde_json::to_vec(&projected).unwrap().len() < 4_000);
    let plan = PlanDefinition {
        id: digest(b"plan"),
        identity: NativePlanIdentity {
            plan_id: "9".into(),
            query: identity,
            qualifiers: [
                ("plan_hash".into(), "FEDCBA9876543210".into()),
                ("initial_compile_at".into(), "2026-11-11T06:00:01Z".into()),
            ]
            .into(),
        },
        query_definition_id: Some(query.id),
        source_text_bytes: None,
        plan: DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted },
        engine_version: Some("native-version".into()),
        native_properties: [("compatibility_level".into(), "170".to_string())].into(),
    };
    let projected = wire(
        &Schema::from_logical(culprit_collector::analytical::schema::definitions::plans(&[]).unwrap()).unwrap(),
        plan,
    );
    assert_eq!(projected["PlanPayloadState"], "unavailable");
    assert_eq!(projected["PlanPayloadUnavailableReason"], "restricted");
    assert_eq!(projected["HasPlanPayloadSha256"], false);
    assert!(projected["NativeSourceTextBytes"].is_null());
}

#[test]
fn resource_metrics_preserve_scope_units_aggregation_and_unknown_values() {
    use culprit_core::telemetry::*;
    let metric = ResourceMetricSample {
        key: "cpu".into(),
        unit: ResourceUnit::Percent,
        scope: culprit_core::MeasurementScope::Database,
        statistic: ResourceStatistic::Maximum,
        period_ms: Some(15000),
        limit_basis: ResourceLimitBasis::DatabaseLimit,
        value: None,
    };
    let native =
        ResourceMeasurement { at: "2026-11-11T06:00:00Z".into(), replica_id: "primary".into(), metrics: vec![metric] };
    let record = TelemetryRecord::ResourceSample(native);
    let mut input = json!({"record":record});
    let (manifest, _) = culprit_collector::analytical::test_support::fixture();
    culprit_collector::analytical::prepared::prepare(&mut input, &record, &manifest.source.target).unwrap();
    let schema = Schema::from_logical(culprit_collector::analytical::schema::samples::resources(&[]).unwrap()).unwrap();
    let projected: Value = serde_json::from_slice(&schema.encode(&schema.project(&input).unwrap()).unwrap()).unwrap();
    let metrics: Value = serde_json::from_str(projected["Metrics"].as_str().unwrap()).unwrap();
    assert_eq!(metrics[0]["scope"], "database");
    assert_eq!(metrics[0]["unit"], "percent");
    assert_eq!(metrics[0]["statistic"], "maximum");
    assert_eq!(metrics[0]["period_ms"], 15000);
    assert_eq!(metrics[0]["describes_subject"], true);
    assert!(metrics[0]["value"].is_null());
}

#[test]
fn wait_counter_projection_preserves_unknown_reset_and_exact_cumulative_totals() {
    use culprit_core::telemetry::*;
    let native = WaitCounterObservation {
        source: "database_waits".into(),
        wait_type: "LCK_M_X".into(),
        scope: culprit_core::MeasurementScope::Database,
        reset: culprit_core::ResetMarker::Unknown,
        waiting_tasks_count: "9007199254740993".into(),
        wait_time_ms: "101".into(),
        signal_wait_time_ms: "7".into(),
        max_wait_time_ms: "23".into(),
    };
    assert!(native.is_valid());
    let projected = wire(
        &Schema::from_logical(culprit_collector::analytical::schema::samples::wait_counters(&[]).unwrap()).unwrap(),
        native,
    );
    assert_eq!(projected["ResetKind"], "unknown");
    assert_eq!(projected["HasResetValue"], false);
    assert_eq!(projected["WaitingTasks"], "9007199254740993");
    assert_eq!(projected["WaitTimeMs"], "101");
    assert_eq!(projected["SignalWaitTimeMs"], "7");
    assert_eq!(projected["MaxWaitTimeMs"], "23");
}

#[test]
fn settings_keep_configured_active_and_absent_default_values_separate() {
    use culprit_core::telemetry::*;
    let native = SettingObservation {
        source: "configuration".into(),
        namespace: "database".into(),
        name: "MAXDOP".into(),
        scope: culprit_core::MeasurementScope::Database,
        nature: SettingNature::Configuration,
        value: Some("0".into()),
        value_in_use: Some("2".into()),
        value_for_secondary: None,
        unit: None,
        native_context: None,
        native_source: None,
        pending_restart: None,
        native_is_default: None,
        default_value: None,
        default_source: SettingDefaultSource::Unknown,
    };
    assert!(native.is_valid());
    let projected = wire(
        &Schema::from_logical(culprit_collector::analytical::schema::samples::settings(&[]).unwrap()).unwrap(),
        native,
    );
    assert_eq!(projected["ConfiguredValue"], "0");
    assert_eq!(projected["ActiveValue"], "2");
    assert_eq!(projected["HasConfiguredValue"], true);
    assert_eq!(projected["HasDefaultValue"], false);
    assert!(projected["NativeIsDefault"].is_null());
    assert_eq!(projected["DefaultSource"], "unknown");
}
