use super::*;
use crate::adx::{catalog::SchemaCatalog, schema};

fn activity() -> ActivityObservation {
    ActivityObservation {
        key: ActivityKey {
            session: NativeActivityIdentity { id: "61".into(), generation: "login-1".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "request-1".into() }),
            transaction: None,
        },
        idle: false,
        scope: ActivityScopeBasis::RequestDatabase,
        state: "suspended".into(),
        database: Some("db".into()),
        command: Some("SELECT".into()),
        last_request_age_ms: None,
        open_transactions: 1,
        elapsed_ms: Some("9007199254740993".into()),
        transaction_age_ms: None,
        session_counters: ActivityCounters { cpu_ms: Some("11".into()), ..Default::default() },
        request_counters: Some(ActivityCounters { cpu_ms: Some("0".into()), ..Default::default() }),
        wait: Some(ActivityWait {
            resources: None,
            native_type: Some("LCK_M_X".into()),
            elapsed_ms: Some("40000".into()),
            owners: vec![ActivityWaitOwner::Session { id: "62".into(), generation: None }],
        }),
        query_hash: None,
        client: Some(ActivityClient { application: Some("collector-name".into()), user: None, host: None }),
        statement: DefinitionPayload::sql(&format!("SELECT {};", "字段".repeat(1000)), culprit_core::Engine::Mssql),
    }
}
fn input(record: &TelemetryRecord) -> Value {
    let mut input = json!({"record":record});
    prepare(&mut input, record).unwrap();
    input
}
fn wire(schema: schema::Schema, input: &Value) -> Value {
    let values = schema.project(input).unwrap();
    serde_json::from_slice(&schema.encode(&values).unwrap()).unwrap()
}
#[test]
fn every_analytical_stream_has_one_complete_bounded_schema_and_checkpoints_stay_recovery_only() {
    let catalog = SchemaCatalog::new().unwrap();
    let second = SchemaCatalog::new().unwrap();
    assert_eq!(catalog.digest(), second.digest());
    assert_eq!(catalog.schemas().count(), TelemetryStream::ALL.len() - 1);
    let mut tables = std::collections::BTreeSet::new();
    for stream in TelemetryStream::ALL {
        match stream {
            TelemetryStream::ActivityCheckpoints => assert!(catalog.get(stream).is_err()),
            _ => {
                let schema = catalog.get(stream).unwrap();
                assert!(tables.insert(schema.table()));
                assert!(schema.columns().len() <= 128);
                assert!(schema.create_mapping().is_ok());
            }
        }
    }
}
#[test]
fn activity_and_incidents_keep_owner_generations_separate_counters_and_bounded_utf8_sql_references() {
    let activity = activity();
    assert!(activity.is_valid());
    let original = serde_json::to_vec(&activity).unwrap();
    let projected = input(&TelemetryRecord::Activity(activity.clone()));
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
    let change = ActivityIncidentChange {
        id: digest(b"change"),
        at: "2026-11-11T06:05:00Z".into(),
        transition: ActivityIncidentTransition::Resolved,
        state: ActivityIncidentState {
            id: digest(b"incident"),
            first_seen_at: "2026-11-11T06:00:00Z".into(),
            last_seen_at: "2026-11-11T06:04:00Z".into(),
            last_emitted_at: "2026-11-11T06:00:00Z".into(),
            conditions: vec![ActivityCondition::Blocked],
            progress: ActivityProgress::NoObservedCounterChange,
            observation: activity,
        },
    };
    assert!(change.is_valid());
    let projected = wire(schema::activity::incidents(&[]).unwrap(), &input(&TelemetryRecord::Incident(change)));
    assert_eq!(projected["Transition"], "resolved");
    assert_eq!(projected["LastSeenAt"], "2026-11-11T06:04:00.000000000Z");
    assert_eq!(projected["ChangedAt"], "2026-11-11T06:05:00.000000000Z");
    assert_eq!(projected["Progress"], "no-observed-counter-change");
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
