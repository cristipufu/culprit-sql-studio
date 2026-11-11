use super::*;

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
            row_id: "1".into(),
            plan_id: "2".into(),
            replica_group_id: "3".into(),
            execution_type: 4,
            plan_definition_id: Some("a".repeat(64)),
        },
        executions: "9007199254740993".into(),
        avg_duration_us: 13.5,
        stdev_duration_us: 3.5,
        min_duration_us: 2.5,
        max_duration_us: 24.5,
        avg_cpu_us: 7.5,
        avg_logical_reads_8kb_pages: 11.5,
        avg_physical_reads_8kb_pages: 1.5,
        avg_logical_writes_8kb_pages: None,
        avg_rows: Some(17.5),
        avg_dop: Some(4.5),
        avg_max_used_memory_8kb_pages: Some(19.5),
        avg_clr_us: Some(5.5),
        avg_log_bytes: Some(23.5),
        avg_tempdb_8kb_pages: Some(29.5),
        first_execution_at: "2026-11-11T06:00:00Z".into(),
        last_execution_at: "2026-11-11T06:01:00Z".into(),
    };
    let schema = queries::runtime(&[]).unwrap();
    let input = json!({"record":{"data":native}});
    let projected = schema.project(&input).unwrap();
    let wire: Value = serde_json::from_slice(&schema.encode(&projected).unwrap()).unwrap();
    assert_eq!(wire["Executions"], "9007199254740993");
    assert_eq!(wire["AvgDurationUs"], 13.5);
    assert_eq!(wire["AvgCpuUs"], 7.5);
    assert_eq!(wire["AvgLogicalReads8KbPages"], 11.5);
    assert_eq!(wire["AvgLogBytes"], 23.5);
    assert!(wire["AvgLogicalWrites8KbPages"].is_null());
    let mapped = schema.fields.iter().map(|field| field.pointer).collect::<BTreeSet<_>>();
    for (name, value) in input["record"]["data"].as_object().unwrap() {
        if name == "key" {
            for key in value.as_object().unwrap().keys() {
                assert!(mapped.contains(format!("/record/data/key/{key}").as_str()), "unmapped native key {key}");
            }
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
            row_id: "1".into(),
            plan_id: "2".into(),
            replica_group_id: "3".into(),
            execution_type: 0,
            plan_definition_id: Some("a".repeat(64)),
        },
        category: 7,
        total_wait_ms: "9007199254740993".into(),
        avg_wait_ms: 13.5,
        min_wait_ms: "2".into(),
        max_wait_ms: "23".into(),
    };
    let schema = queries::waits(&[]).unwrap();
    let projected = schema.project(&json!({"record":{"data":native}})).unwrap();
    let wire: Value = serde_json::from_slice(&schema.encode(&projected).unwrap()).unwrap();
    assert_eq!(wire["WaitCategory"], 7);
    assert_eq!(wire["TotalWaitMs"], "9007199254740993");
    assert_eq!(wire["AvgWaitMs"], 13.5);
    assert_eq!(wire["MinWaitMs"], "2");
    assert_eq!(wire["MaxWaitMs"], "23");
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
        context_settings_id: "3".into(),
        object_id: "0".into(),
        query_hash: "0123456789ABCDEF".into(),
        initial_compile_at: "2026-11-11T06:00:00Z".into(),
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
    let projected = wire(&definitions::queries(&[]).unwrap(), &query);
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
            plan_hash: "FEDCBA9876543210".into(),
            initial_compile_at: "2026-11-11T06:00:01Z".into(),
        },
        query_definition_id: query.id,
        engine_version: "native-version".into(),
        compatibility_level: 170,
        source_text_bytes: None,
        plan: DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted },
    };
    let projected = wire(&definitions::plans(&[]).unwrap(), plan);
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
    let projected = wire(&samples::resources(&[]).unwrap(), native);
    let metrics: Value = serde_json::from_str(projected["Metrics"].as_str().unwrap()).unwrap();
    assert_eq!(metrics[0]["scope"], "database");
    assert_eq!(metrics[0]["unit"], "percent");
    assert_eq!(metrics[0]["statistic"], "maximum");
    assert_eq!(metrics[0]["period_ms"], 15000);
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
    let projected = wire(&samples::wait_counters(&[]).unwrap(), native);
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
        native_is_default: None,
        default_value: None,
        default_source: SettingDefaultSource::Unknown,
    };
    assert!(native.is_valid());
    let projected = wire(&samples::settings(&[]).unwrap(), native);
    assert_eq!(projected["ConfiguredValue"], "0");
    assert_eq!(projected["ActiveValue"], "2");
    assert_eq!(projected["HasConfiguredValue"], true);
    assert_eq!(projected["HasDefaultValue"], false);
    assert!(projected["NativeIsDefault"].is_null());
    assert_eq!(projected["DefaultSource"], "unknown");
}
