//! Synthetic native tuning facts for physical schema and receipt conformance, without an SDK decoder or native work.
use super::*;
use culprit_core::{Engine, QueryStoreScope, source_id};
use serde_json::{Map, Value};

pub fn tuning_scope_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let scopes = [
        QueryStoreScope::Local,
        QueryStoreScope::ReplicaGroup { id: "7".into() },
        QueryStoreScope::ReplicaGroup { id: "8".into() },
        QueryStoreScope::Unknown,
    ];
    let mut records = Vec::new();
    for scope in scopes {
        records.push(TelemetryRecord::Tuning(TuningObservation {
            source: source_id::FORCED_PLANS.into(),
            fact: TuningFact::ForcedPlan(NativeForcedPlan {
                plan_scope: scope.clone(),
                query_id: "3".into(),
                plan_id: "9".into(),
                failures: "9007199254740993".into(),
                last_failure_reason: None,
                last_execution_at: Some("2020-01-01T06:00:00Z".into()),
                forcing: Some("MANUAL".into()),
                reset: culprit_core::ResetMarker::Unknown,
            }),
        }));
        records.push(TelemetryRecord::Tuning(TuningObservation {
            source: source_id::TUNING_RECOMMENDATIONS.into(),
            fact: TuningFact::Recommendation(Box::new(NativeTuningRecommendation {
                plan_scope: scope,
                name: "PR_native".into(),
                kind: "FORCE_LAST_GOOD_PLAN".into(),
                reason: None,
                state: Some("Active".into()),
                state_reason: None,
                score: Some(90),
                query_id: Some("3".into()),
                regressed: TuningPlanStatistics {
                    plan_id: Some("9".into()),
                    executions: Some("9007199254740993".into()),
                    aborted: None,
                    avg_cpu_us: Some(10.5),
                    stdev_cpu_us: None,
                },
                recommended: TuningPlanStatistics {
                    plan_id: Some("8".into()),
                    executions: Some("2".into()),
                    aborted: Some("0".into()),
                    avg_cpu_us: Some(2.5),
                    stdev_cpu_us: Some(0.0),
                },
                script: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
                index: None,
                valid_since_without_offset: None,
                last_refresh_without_offset: None,
                applied_without_offset: None,
                valid_since_utc_estimate: None,
                applied_utc_estimate: None,
                last_execution_at: Some("2020-01-01T06:00:00Z".into()),
                is_executable: None,
                is_revertable: None,
            })),
        }));
    }
    let source = CollectionDescription {
        partition: SourcePartition::Target,
        target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "fixture", "fixture.invalid", 1433, None).into(),
        source: "native-tuning-scope-fixture".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Tuning,
            observed_from: "2020-01-01T06:00:00Z".into(),
            observed_until: "2020-01-01T06:00:01Z".into(),
            records: records.len() as u32,
            sources: [source_id::FORCED_PLANS, source_id::TUNING_RECOMMENDATIONS]
                .map(|source| StateSourceCoverage::observed(source.into(), 4, false))
                .to_vec(),
        }),
    };
    (source, records)
}

/// Compare normalized physical values to the retained native fact. This is not a new readback decoder.
pub fn assert_tuning_scope_projection(row: &ExportedRecord, logical: &Map<String, Value>) {
    let TelemetryRecord::Tuning(observation) = &row.record else { panic!("tuning observation") };
    let fact = serde_json::to_value(&observation.fact).unwrap();
    assert_eq!(logical["NativeSource"], observation.source);
    assert_eq!(logical["FactKind"], fact["kind"]);
    assert_eq!(logical["FactKey"], observation.fact.key());
    let mut expected_details = fact["data"].clone();
    for field in row.record.definition_payloads() {
        let pointer = field.pointer.strip_prefix("/record/data/fact/data").expect("native tuning payload");
        let reference = logical["Details"].pointer(pointer).expect("projected native payload reference");
        assert_eq!(reference["pointer"], field.pointer);
        // These fixtures retain unavailable payloads. Preparation adds their exact recovery pointer without
        // changing the native unavailable reason or any plan scope, counter, query or plan reference.
        assert!(matches!(field.payload, DefinitionPayload::Unavailable { .. }));
        expected_details.pointer_mut(pointer).unwrap()["pointer"] = Value::String(field.pointer.into());
    }
    assert_eq!(logical["Details"], expected_details);
    assert_eq!(logical["DetailSha256"], digest(&serde_json::to_vec(&expected_details).unwrap()));
    let record_id = row.record_id.as_deref().expect("a native tuning point has a record identity");
    assert_eq!(logical["RecordId"], record_id);
    assert_eq!(logical["NativePayloadDigest"], RowReceipt::from_export(row).unwrap().payload_digest);
}

pub fn change_tuning_plan_scope(row: &mut ExportedRecord) {
    let TelemetryRecord::Tuning(observation) = &mut row.record else { panic!("tuning observation") };
    let scope = QueryStoreScope::ReplicaGroup { id: "99".into() };
    match &mut observation.fact {
        TuningFact::ForcedPlan(value) => value.plan_scope = scope,
        TuningFact::Recommendation(value) => value.plan_scope = scope,
        _ => panic!("scoped native plan fact"),
    }
}
