//! Exact-count native populations for shared transport and pure runtime-analysis conformance.
use super::*;
use crate::analytical::evidence::EvidenceCapture;
use culprit_core::QueryVariantFamily;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryCohortCase {
    PlanVariation,
    ConvergedPlans,
    UnknownCohort,
    OtherContext,
    OtherObject,
    OtherLifetime,
    OtherOutcome,
    FutureOutcome,
    OtherReplica,
    OtherNativeDimension,
    MissingPlan,
    RestrictedStatement,
    HealthyVariantFamily,
}

pub fn query_cohort_evidence_cases() -> Vec<(QueryCohortCase, CollectionDescription, Vec<TelemetryRecord>)> {
    use QueryCohortCase::*;
    [
        PlanVariation,
        ConvergedPlans,
        UnknownCohort,
        OtherContext,
        OtherObject,
        OtherLifetime,
        OtherOutcome,
        FutureOutcome,
        OtherReplica,
        OtherNativeDimension,
        MissingPlan,
        RestrictedStatement,
        HealthyVariantFamily,
    ]
    .into_iter()
    .map(|case| {
        let (source, records) = query_cohort_evidence(case);
        (case, source, records)
    })
    .collect()
}

pub fn query_cohort_evidence(case: QueryCohortCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut source = super::queries::source();
    source.source = "qualified-runtime-populations".into();
    let SourceObservation::QueryHistory(history) = &mut source.observation else { unreachable!() };
    history.dimensions = [
        ("replica_group_id".into(), vec!["0".into(), "1".into()]),
        ("execution_type".into(), vec!["0".into(), "3".into(), "4".into(), "203".into()]),
        ("native-shard".into(), vec!["primary".into(), "secondary".into()]),
    ]
    .into();
    history.waits = Some(super::wait_semantics(WaitCaptureState::Enabled));
    let identity = NativeQueryIdentity {
        query_id: "17".into(),
        qualifiers: [
            ("context_settings_id".into(), "5".into()),
            ("object_id".into(), "0".into()),
            ("query_hash".into(), "0123456789ABCDEF".into()),
            ("initial_compile_at".into(), "2020-01-01T05:00:00Z".into()),
        ]
        .into(),
    };
    let query = QueryDefinition {
        id: identity.definition_id(&source).unwrap(),
        identity,
        variant_family: None,
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        context: [("native-option".into(), "enabled".into())].into(),
        context_available: true,
        source_text_bytes: None,
        sql: DefinitionPayload::sql("SELECT payload FROM public.orders WHERE customer_id=9264", source.target.engine()),
    };
    let slow = plan(&source, &query, "47");
    let fast = plan(&source, &query, "8");
    let converged = matches!(case, QueryCohortCase::ConvergedPlans | QueryCohortCase::HealthyVariantFamily);
    let mut records = vec![
        TelemetryRecord::QueryDefinition(query.clone()),
        TelemetryRecord::PlanDefinition(slow.clone()),
        TelemetryRecord::PlanDefinition(fast.clone()),
        runtime(&query, &slow, "fragment-1", "1", if converged { 10.0 } else { 100.0 }),
        runtime(&query, &slow, "fragment-2", "9", if converged { 10.0 } else { 1000.0 }),
        runtime(&query, &fast, "fragment-3", "40", 10.0),
        wait(&query, &slow, "wait-1", "25"),
        wait(&query, &fast, "wait-2", "5"),
    ];
    match case {
        QueryCohortCase::UnknownCohort => {
            for key in records.iter_mut().filter_map(measurement_key) {
                key.cohort = None;
            }
        }
        QueryCohortCase::OtherContext => {
            replace_fast_context(&source, &query, &fast, &mut records, "context_settings_id", "6")
        }
        QueryCohortCase::OtherObject => replace_fast_context(&source, &query, &fast, &mut records, "object_id", "1999"),
        QueryCohortCase::OtherLifetime => {
            replace_fast_context(&source, &query, &fast, &mut records, "initial_compile_at", "2020-01-01T05:00:01Z")
        }
        QueryCohortCase::OtherOutcome => replace_fast_dimension(&mut records, "execution_type", "3"),
        QueryCohortCase::FutureOutcome => replace_fast_dimension(&mut records, "execution_type", "203"),
        QueryCohortCase::OtherReplica => replace_fast_dimension(&mut records, "replica_group_id", "1"),
        QueryCohortCase::OtherNativeDimension => replace_fast_dimension(&mut records, "native-shard", "secondary"),
        QueryCohortCase::MissingPlan => {
            records.retain(|row| !matches!(row, TelemetryRecord::PlanDefinition(plan) if plan.identity.plan_id == "8"));
            for key in records.iter_mut().filter_map(measurement_key) {
                if let Some(plan) = &mut key.references.plan
                    && plan.native.id == "8"
                {
                    plan.definition_id = None;
                }
            }
        }
        QueryCohortCase::RestrictedStatement => {
            let TelemetryRecord::QueryDefinition(query) = &mut records[0] else { unreachable!() };
            query.sql = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
        }
        QueryCohortCase::HealthyVariantFamily => {
            let TelemetryRecord::QueryDefinition(query) = &mut records[0] else { unreachable!() };
            let parent = NativeQueryIdentity { query_id: "6".into(), qualifiers: query.identity.qualifiers.clone() };
            query.variant_family = Some(QueryVariantFamily {
                variant: query.identity.clone(),
                parent: parent.clone(),
                dispatcher: NativePlanIdentity {
                    plan_id: "9".into(),
                    query: parent,
                    qualifiers: [("initial_compile_at".into(), "2020-01-01T05:00:00Z".into())].into(),
                },
            });
        }
        QueryCohortCase::PlanVariation | QueryCohortCase::ConvergedPlans => {}
    }
    (source, records)
}

fn plan(source: &CollectionDescription, query: &QueryDefinition, id: &str) -> PlanDefinition {
    let identity = NativePlanIdentity {
        plan_id: id.into(),
        query: query.identity.clone(),
        qualifiers: [("native-plan-generation".into(), "original".into())].into(),
    };
    PlanDefinition {
        id: identity.definition_id(source).unwrap(),
        identity,
        query_definition_id: Some(query.id.clone()),
        engine_version: None,
        native_properties: Default::default(),
        source_text_bytes: None,
        plan: DefinitionPayload::plan(
            r#"{"Plan":{"Node Type":"Seq Scan","Relation Name":"orders"}}"#,
            source.target.engine(),
        ),
    }
}

fn key(query: &QueryDefinition, plan: &PlanDefinition, id: &str) -> NativeMeasurementKey {
    let cohort = NativeIdentity::new("native-execution-population")
        .qualified("replica_group_id", "0")
        .qualified("execution_type", "0")
        .qualified("native-shard", "primary");
    let mut native = cohort.clone();
    native.id = id.into();
    native.qualifiers.insert("plan_id".into(), plan.identity.plan_id.clone());
    NativeMeasurementKey { native, cohort: Some(cohort), references: references(query, plan) }
}

fn references(query: &QueryDefinition, plan: &PlanDefinition) -> StatementReferences {
    StatementReferences {
        query: Some(NativeDefinitionReference {
            native: query.identity.native(),
            definition_id: Some(query.id.clone()),
        }),
        plan: Some(NativeDefinitionReference { native: plan.identity.native(), definition_id: Some(plan.id.clone()) }),
    }
}

fn runtime(query: &QueryDefinition, plan: &PlanDefinition, id: &str, count: &str, mean_ms: f64) -> TelemetryRecord {
    TelemetryRecord::QueryRuntime(RuntimeMeasurement {
        key: key(query, plan, id),
        executions: count.into(),
        metrics: vec![
            RuntimeMetric {
                minimum: Some(mean_ms),
                maximum: Some(mean_ms),
                standard_deviation: Some(0.0),
                ..RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Milliseconds, mean_ms)
            },
            RuntimeMetric::average(RuntimeMetricKey::CpuTime, RuntimeMetricUnit::Milliseconds, mean_ms / 2.0),
        ],
        page_server_reads: None,
        first_execution_at: Some("2020-01-01T06:01:00Z".into()),
        last_execution_at: Some("2020-01-01T06:59:00Z".into()),
    })
}

fn wait(query: &QueryDefinition, plan: &PlanDefinition, id: &str, total_ms: &str) -> TelemetryRecord {
    TelemetryRecord::QueryWaits(WaitMeasurement {
        key: key(query, plan, id),
        category: "native-lock".into(),
        unit: RuntimeMetricUnit::Milliseconds,
        total: MeasurementValue::Integer(total_ms.into()),
        average: None,
        minimum: None,
        maximum: None,
    })
}

fn measurement_key(row: &mut TelemetryRecord) -> Option<&mut NativeMeasurementKey> {
    match row {
        TelemetryRecord::QueryRuntime(row) => Some(&mut row.key),
        TelemetryRecord::QueryWaits(row) => Some(&mut row.key),
        _ => None,
    }
}

fn replace_fast_dimension(records: &mut [TelemetryRecord], dimension: &str, value: &str) {
    for key in records.iter_mut().filter_map(measurement_key).filter(|key| key.native.qualifiers["plan_id"] == "8") {
        key.native.qualifiers.insert(dimension.into(), value.into());
        key.cohort.as_mut().unwrap().qualifiers.insert(dimension.into(), value.into());
    }
}

fn replace_fast_context(
    source: &CollectionDescription,
    query: &QueryDefinition,
    plan: &PlanDefinition,
    records: &mut Vec<TelemetryRecord>,
    qualifier: &str,
    value: &str,
) {
    let mut query = query.clone();
    query.identity.qualifiers.insert(qualifier.into(), value.into());
    query.id = query.identity.definition_id(source).unwrap();
    let mut plan = plan.clone();
    plan.identity.query = query.identity.clone();
    plan.id = plan.identity.definition_id(source).unwrap();
    plan.query_definition_id = Some(query.id.clone());
    for record in records.iter_mut() {
        if matches!(record, TelemetryRecord::PlanDefinition(old) if old.identity.plan_id == "8") {
            *record = TelemetryRecord::PlanDefinition(plan.clone());
        } else if let Some(key) = measurement_key(record).filter(|key| key.native.qualifiers["plan_id"] == "8") {
            key.references = references(&query, &plan);
        }
    }
    records.push(TelemetryRecord::QueryDefinition(query));
}

pub fn assert_query_cohort_capture(
    capture: &EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    assert_eq!(capture.source(), source);
    let expected = records
        .iter()
        .filter(|record| capture.families().iter().any(|family| family.stream() == record.stream()))
        .collect::<Vec<_>>();
    assert_eq!(capture.records().len(), expected.len());
    for record in expected {
        let expected = ExportedRecord::new(source, capture.capture(), record.clone()).unwrap();
        assert_eq!(capture.records().iter().filter(|row| row.row() == &expected).count(), 1);
    }
}
