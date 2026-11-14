use super::*;
use crate::analytical::{catalog::LogicalCatalog, schema};

mod investigations;

fn input(record: &TelemetryRecord) -> Value {
    let mut input = json!({"record":record});
    let target = crate::analytical::test_support::fixture().0.source.target;
    prepare(&mut input, record, &target).unwrap();
    input
}

#[test]
fn resource_projection_distinguishes_subject_usage_from_shared_context_without_changing_native_evidence() {
    let (_, row) = crate::analytical::test_support::fixture();
    let TelemetryRecord::ResourceSample(mut sample) = row.record else { panic!("resource fixture") };
    sample.metrics[0].limit_basis = ResourceLimitBasis::PoolLimit;
    let mut shared = sample.metrics[0].clone();
    shared.scope = culprit_core::MeasurementScope::Server;
    shared.limit_basis = ResourceLimitBasis::InstanceLimit;
    shared.value = Some(80.0);
    sample.metrics.push(shared);
    let record = TelemetryRecord::ResourceSample(sample);
    let prepared = input(&record);
    assert_eq!(prepared["_projection"]["resource_metrics"][0]["describes_subject"], true);
    assert_eq!(prepared["_projection"]["resource_metrics"][1]["describes_subject"], false);
    assert_eq!(prepared["record"], serde_json::to_value(&record).unwrap());
    assert!(prepared["record"]["data"]["metrics"][0].get("describes_subject").is_none());
}
#[test]
fn diagnostic_details_omit_only_the_private_target_while_native_recovery_evidence_stays_exact() {
    for (source, records) in crate::analytical::test_support::diagnostic_evidence_cases() {
        let record = &records[0];
        let original = serde_json::to_value(record).unwrap();
        let original_bytes = serde_json::to_vec(record).unwrap();
        let prepared = input(record);
        let mut public_record = original.clone();
        public_record["data"]["fact"]["data"]["episode"]["scope"].as_object_mut().unwrap().remove("target");
        assert_eq!(prepared["record"], public_record);
        assert_eq!(serde_json::to_vec(record).unwrap(), original_bytes);
        let projected = wire(schema::facts::operational(&[]).unwrap(), &prepared);
        let details: Value = serde_json::from_str(projected["Details"].as_str().unwrap()).unwrap();
        let mut expected = original["data"]["fact"]["data"].clone();
        assert_eq!(expected["episode"]["scope"]["target"], serde_json::to_value(&source.target).unwrap());
        expected["episode"]["scope"].as_object_mut().unwrap().remove("target");
        assert_eq!(details, expected, "only the duplicated private target leaves warehouse Details");
        let encoded = serde_json::to_string(&prepared).unwrap();
        assert!(!encoded.contains("private-diagnostic"));
        assert!(!encoded.contains("/private/diagnostics"));
    }
}
fn wire(schema: schema::LogicalSchema, input: &Value) -> Value {
    let values = schema.project(input).unwrap();
    serde_json::from_slice(&schema.encode(&values).unwrap()).unwrap()
}
#[test]
fn every_analytical_stream_has_one_complete_bounded_schema_and_checkpoints_stay_recovery_only() {
    let catalog = LogicalCatalog::new().unwrap();
    let second = LogicalCatalog::new().unwrap();
    assert_eq!(catalog.digest(), second.digest());
    assert_eq!(
        catalog.schemas().count(),
        TelemetryStream::ALL.into_iter().filter(|stream| stream.delivery_required()).count()
    );
    let mut tables = std::collections::BTreeSet::new();
    for stream in TelemetryStream::ALL {
        match stream {
            TelemetryStream::DiagnosticCheckpoints => {
                assert!(catalog.get(stream).is_err())
            }
            _ => {
                let schema = catalog.get(stream).unwrap();
                assert!(tables.insert(schema.table()));
                assert!(schema.columns().len() <= 128);
                assert!(schema.columns().iter().all(|column| !column.name.starts_with("Has")));
            }
        }
    }
}
#[test]
fn native_activity_keeps_owner_generations_separate_counters_and_bounded_utf8_sql_references() {
    let mut activity = crate::analytical::test_support::activity_observation(DefinitionPayload::sql(
        &format!("SELECT {} FROM dbo.orders WHERE token='private-activity-value';", "字段".repeat(1000)),
        culprit_core::Engine::Mssql,
    ));
    activity
        .wait
        .as_mut()
        .unwrap()
        .owners
        .push(ActivityWaitOwner::Session { id: "63".into(), generation: Some("observed-owner-generation".into()) });
    assert!(activity.is_valid());
    let original = serde_json::to_vec(&activity).unwrap();
    let projected = input(&TelemetryRecord::from(activity.clone()));
    let payload = &projected["_projection"]["payloads"][0];
    assert_eq!(payload["pointer"], "/record/data/statement");
    assert_eq!(payload["preview_truncated"], true);
    assert!(payload["preview"].as_str().unwrap().len() <= MAX_PREVIEW_BYTES);
    assert!(projected["record"]["data"]["statement"]["text"].is_null());
    assert_eq!(serde_json::to_vec(&activity).unwrap(), original);
    let projected = wire(schema::activity::observations(&[]).unwrap(), &projected);
    assert_eq!(projected["ElapsedMs"], "9007199254740993");
    assert_eq!(projected["SessionCpuMs"], "11");
    assert_eq!(projected["RequestCpuMs"], "0");
    assert!(projected["RequestWrites"].is_null());
    assert_eq!(
        serde_json::from_str::<Value>(projected["WaitOwners"].as_str().unwrap()).unwrap()[0]["generation"],
        Value::Null
    );
    assert_eq!(projected["ClientApplication"], "collector-name");
    let owners: Value = serde_json::from_str(projected["WaitOwners"].as_str().unwrap()).unwrap();
    assert_eq!(owners[1]["generation"], "observed-owner-generation");
    assert!(!serde_json::to_string(&projected).unwrap().contains("private-activity-value"));
}

#[test]
fn activity_facts_keep_unknowns_and_native_statement_identity_without_copying_full_sql_into_projection() {
    let (source, records) = crate::analytical::test_support::activity_evidence_cases().remove(0);
    let TelemetryRecord::Activity(native) = &records[0] else { panic!() };
    let prepared = input(&records[0]);
    let projection = wire(schema::activity::observations(&[]).unwrap(), &prepared);
    let facts: Value = serde_json::from_str(projection["ActivityFacts"].as_str().unwrap()).unwrap();
    let token: Value = serde_json::from_str(projection["StatementIdentity"].as_str().unwrap()).unwrap();
    assert_eq!(token, serde_json::to_value(native.statement_identity.as_ref().unwrap()).unwrap());
    assert_eq!(facts["statement_identity"], token);
    assert!(facts["open_transactions"].is_null());
    assert_eq!(facts["session_counters"]["logical_reads"], "18446744073709551615");
    assert_eq!(facts["client"]["user"], "");
    assert!(facts["client"]["host"].is_null());
    assert!(facts["statement"].get("text").is_none());
    assert_eq!(facts["statement"]["pointer"], "/record/data/statement");
    assert!(!serde_json::to_string(&projection).unwrap().contains("private-activity-canary"));
    assert!(source.observation.activity().unwrap().is_valid());
}

#[test]
fn activity_lock_wait_verdict_projects_unknown_and_native_negative_separately() {
    let mut activity = crate::analytical::test_support::activity_observation(DefinitionPayload::Unavailable {
        reason: PayloadUnavailable::NotCollected,
    });
    for verdict in [None, Some(false), Some(true)] {
        activity.waiting_on_lock = verdict;
        let projected = input(&TelemetryRecord::from(activity.clone()));
        let projection = wire(schema::activity::observations(&[]).unwrap(), &projected);
        let expected = serde_json::to_value(verdict).unwrap();
        assert_eq!(projection["WaitingOnLock"], expected);
        let facts: Value = serde_json::from_str(projection["ActivityFacts"].as_str().unwrap()).unwrap();
        assert_eq!(facts["waiting_on_lock"], expected);
    }
}

#[test]
fn catalog_expressions_and_tuning_bodies_are_references_with_their_native_metadata_intact() {
    let column =
        serde_json::from_str::<MssqlColumnDefinition>(include_str!("../../../../../fixtures/telemetry-column.json"))
            .unwrap();
    let fact = ObjectDefinitionFact::MssqlColumn(Box::new(column));
    let row = ObjectDefinitionRecord::new("columns".into(), fact).unwrap();
    let projected = wire(schema::facts::objects(&[]).unwrap(), &input(&TelemetryRecord::ObjectDefinition(row.clone())));
    assert_eq!(projected["DefinitionVersion"], row.version);
    let details: Value = serde_json::from_str(projected["Details"].as_str().unwrap()).unwrap();
    assert_eq!(details["default"]["payload"]["reason"], "oversized");
    assert_eq!(details["max_length"], 80);
    let hint = NativeQueryHint {
        hint_id: "1".into(),
        query_id: "7".into(),
        scope: culprit_core::QueryStoreHintScope::Local,
        hint: DefinitionPayload::sql(&"x".repeat(200_000), culprit_core::Engine::Mssql),
        failures: "9007199254740993".into(),
        last_failure_reason: None,
        native_source: None,
        native_source_name: None,
        reset: culprit_core::ResetMarker::Unknown,
    };
    let row =
        TelemetryRecord::Tuning(TuningObservation { source: "query-hints".into(), fact: TuningFact::QueryHint(hint) });
    assert!(row.is_valid());
    let projected = wire(schema::facts::tuning(&[]).unwrap(), &input(&row));
    assert_eq!(projected["DetailState"], "inline");
    let details: Value = serde_json::from_str(projected["Details"].as_str().unwrap()).unwrap();
    assert_eq!(details["failures"], "9007199254740993");
    assert_eq!(details["hint"]["bytes"], 200_000);
    assert!(details["hint"]["text"].is_null());
}
#[test]
fn large_deadlock_graphs_keep_signal_fields_and_explicit_full_evidence_references() {
    let mut row = DeadlockEvent {
        position: digest(b"event"),
        file_id: digest(b"file"),
        file_offset: "18446744073709551615".into(),
        graph_ordinal: 2,
        at: "2026-11-11T06:00:00Z".into(),
        pattern: culprit_core::DeadlockPattern::Cycle,
        processes: vec![culprit_core::DeadlockProcess {
            id: "p1".into(),
            victim: true,
            statement: Some("x".repeat(200_000)),
            ..Default::default()
        }],
        resources: vec![],
    };
    let projected = wire(schema::facts::deadlocks(&[]).unwrap(), &input(&TelemetryRecord::Deadlock(row.clone())));
    assert_eq!(projected["DetailState"], "inline");
    let details: Value = serde_json::from_str(projected["Details"].as_str().unwrap()).unwrap();
    assert_eq!(details["processes"][0]["statement"]["bytes"], 200_000);
    row.resources = vec![
        culprit_core::DeadlockResource {
            id: "r1".into(),
            kind: "keylock".into(),
            object: Some("x".repeat(2048)),
            ..Default::default()
        };
        256
    ];
    assert!(row.is_valid());
    let projected = wire(schema::facts::deadlocks(&[]).unwrap(), &input(&TelemetryRecord::Deadlock(row)));
    assert_eq!(projected["DetailState"], "recovery-object");
    assert!(projected["Details"].is_null());
    assert_eq!(projected["ProcessCount"], 1);
    assert_eq!(projected["ResourceCount"], 256);
    assert_eq!(projected["VictimCount"], 1);
    assert_eq!(projected["FileOffset"], "18446744073709551615");
}

#[test]
fn cumulative_query_facts_keep_exact_totals_units_and_resets_in_the_analytical_contract() {
    let sample = StatementCounterSample {
        scope: culprit_core::MeasurementScope::Database,
        distributions: vec![],
        identity: NativeQueryIdentity {
            query_id: "fingerprint".into(),
            qualifiers: [("userid".into(), "17".into())].into(),
        },
        observed_at: "2020-01-01T06:00:00Z".into(),
        reset: culprit_core::ResetMarker::Unknown,
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
            StatementCounter {
                key: RuntimeMetricKey::LogicalReads,
                unit: RuntimeMetricUnit::Pages { bytes_per_page: 16384 },
                total: MeasurementValue::Integer("7".into()),
            },
        ],
    };
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: "native-statement-counters".into(),
        key: sample.key().unwrap(),
        fact: OperationalFact::StatementCounters(Box::new(sample.clone())),
    });
    assert!(record.is_valid());
    let schema = schema::facts::operational(&[]).unwrap();
    let prepared = input(&record);
    let projected = wire(schema::facts::operational(&[]).unwrap(), &prepared);
    let details: Value = serde_json::from_str(projected["Details"].as_str().unwrap()).unwrap();
    assert_eq!(projected["FactKind"], "statement-counters");
    assert_eq!(serde_json::from_value::<StatementCounterSample>(details.clone()).unwrap(), sample);
    let before = schema.project(&prepared).unwrap();
    let mut changed = prepared;
    changed["_projection"]["details"]["totals"][2]["unit"]["bytes_per_page"] = json!(8192);
    assert_ne!(schema.project(&changed).unwrap(), before, "readback must prove native units as well as totals");
    changed["_projection"]["details"] = details;
    changed["_projection"]["details"]["reset"] = json!({"kind":"generation","value":"new-epoch"});
    assert_ne!(schema.project(&changed).unwrap(), before, "reset evidence is part of the exact publication proof");
}
