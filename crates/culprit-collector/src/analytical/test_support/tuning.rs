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
        assert_eq!(reference["pointer"], field.pointer.as_ref());
        // These fixtures retain unavailable payloads. Preparation adds their exact recovery pointer without
        // changing the native unavailable reason or any plan scope, counter, query or plan reference.
        assert!(matches!(field.payload, DefinitionPayload::Unavailable { .. }));
        expected_details.pointer_mut(pointer).unwrap()["pointer"] = Value::String(field.pointer.to_string());
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuningCase {
    Complete,
    UnknownScopes,
    UnavailableDefinitions,
    Empty,
    UnavailableSource,
    LargeDefinition,
}

pub fn tuning_evidence_cases() -> Vec<(TuningCase, CollectionDescription, Vec<TelemetryRecord>)> {
    [
        TuningCase::Complete,
        TuningCase::UnknownScopes,
        TuningCase::UnavailableDefinitions,
        TuningCase::Empty,
        TuningCase::UnavailableSource,
    ]
    .into_iter()
    .map(|case| {
        let (source, records) = tuning_evidence(case);
        (case, source, records)
    })
    .collect()
}

pub fn tuning_evidence(case: TuningCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    if case == TuningCase::UnknownScopes {
        return tuning_scope_evidence();
    }
    let (mut source, original) = tuning_scope_evidence();
    let mut records = original[..2].to_vec();
    let TelemetryRecord::Tuning(forced) = &mut records[0] else { unreachable!() };
    let TuningFact::ForcedPlan(value) = &mut forced.fact else { unreachable!() };
    value.last_failure_reason = Some("NO_PLAN".into());
    let TelemetryRecord::Tuning(recommendation) = &mut records[1] else { unreachable!() };
    let TuningFact::Recommendation(value) = &mut recommendation.fact else { unreachable!() };
    value.regressed.aborted = Some("9007199254740993".into());
    value.recommended.aborted = Some("9007199254740992".into());
    value.script = DefinitionPayload::sql("EXEC sys.sp_query_store_force_plan @query_id=3, @plan_id=8", Engine::Mssql);
    records.push(TelemetryRecord::Tuning(TuningObservation {
        source: source_id::TUNING_OPTIONS.into(),
        fact: TuningFact::AutomaticOption(culprit_core::AutomaticTuningOption {
            name: "FORCE_LAST_GOOD_PLAN".into(),
            desired: "ON".into(),
            actual: "OFF".into(),
            reason: Some("DISABLED".into()),
        }),
    }));
    records.push(TelemetryRecord::Tuning(TuningObservation {
        source: source_id::QUERY_HINTS.into(),
        fact: TuningFact::QueryHint(NativeQueryHint {
            query_id: "3".into(),
            hint_id: "1".into(),
            scope: QueryStoreScope::ReplicaGroup { id: "7".into() },
            hint: DefinitionPayload::sql("OPTION(MAXDOP 1)", Engine::Mssql),
            failures: "0".into(),
            last_failure_reason: None,
            native_source: Some(0),
            native_source_name: Some("USER".into()),
            reset: culprit_core::ResetMarker::Unknown,
        }),
    }));
    match case {
        TuningCase::UnavailableDefinitions => {
            for record in &mut records {
                if let TelemetryRecord::Tuning(row) = record {
                    match &mut row.fact {
                        TuningFact::Recommendation(value) => {
                            value.script = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
                        }
                        TuningFact::QueryHint(value) => {
                            value.hint = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
                        }
                        _ => {}
                    }
                }
            }
        }
        TuningCase::LargeDefinition => {
            let TelemetryRecord::Tuning(row) = &mut records[1] else { unreachable!() };
            let TuningFact::Recommendation(value) = &mut row.fact else { unreachable!() };
            let script = format!(
                "SELECT {} FROM dbo.fixture",
                (0..400).map(|index| format!("column_{index}")).collect::<Vec<_>>().join(",")
            );
            value.script = DefinitionPayload::sql(&script, Engine::Mssql);
        }
        TuningCase::Empty | TuningCase::UnavailableSource => records.clear(),
        _ => {}
    }
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
    snapshot.records = records.len() as u32;
    snapshot.sources =
        [source_id::FORCED_PLANS, source_id::TUNING_RECOMMENDATIONS, source_id::TUNING_OPTIONS, source_id::QUERY_HINTS]
            .into_iter()
            .map(|source| {
                if case == TuningCase::UnavailableSource && source == source_id::FORCED_PLANS {
                    return StateSourceCoverage::unavailable(
                        source.into(),
                        culprit_core::ErrorCode::PermissionDenied,
                        None,
                    );
                }
                let count = records
                    .iter()
                    .filter(|record| matches!(record, TelemetryRecord::Tuning(row) if row.source == source))
                    .count() as u32;
                let complete = case != TuningCase::UnavailableDefinitions
                    || !matches!(source, source_id::TUNING_RECOMMENDATIONS | source_id::QUERY_HINTS);
                StateSourceCoverage::observed(source.into(), count, complete)
            })
            .collect();
    (source, records)
}

pub fn assert_tuning_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert!(capture.is_read_complete());
    assert_eq!(capture.source(), source);
    assert_eq!(capture.records().len(), records.len());
    for (index, record) in records.iter().enumerate() {
        let expected = ExportedRecord::new(source, capture.capture(), record.clone()).unwrap();
        let actual = &capture.records()[index];
        assert_eq!(serde_json::to_value(actual.row()).unwrap(), serde_json::to_value(&expected).unwrap());
        assert_eq!(actual.row_ordinal(), u32::try_from(index + 1).unwrap());
        assert_eq!(actual.native_payload_sha256(), RowReceipt::from_export(&expected).unwrap().payload_digest);
    }
}
