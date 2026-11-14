//! One native query closure exercises both warehouse readers without provider-specific reconstruction.
use super::*;

pub(super) fn source() -> CollectionDescription {
    let (mut manifest, _) = fixture();
    let CollectionTarget::Database(database) = &mut manifest.source.target else { panic!("database fixture") };
    database.engine = culprit_core::Engine::Postgres;
    manifest.source.source = "native-history-fixture".into();
    manifest.source.observation = SourceObservation::QueryHistory(QueryHistoryDescription {
        waits: None,
        interval: TelemetryInterval {
            native_id: "native-period".into(),
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T07:00:00Z".into(),
        },
        dimensions: Default::default(),
        native_properties: Default::default(),
    });
    manifest.source
}

pub fn query_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut source = source();
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
    history.dimensions.insert("execution_type".into(), vec!["0".into(), "3".into(), "4".into()]);
    let identity = NativeQueryIdentity {
        query_id: "native-query-17".into(),
        qualifiers: [("userid".into(), "17".into()), ("context".into(), "native-context-2".into())].into(),
    };
    let plan_identity = NativePlanIdentity {
        plan_id: "native-plan-42".into(),
        query: identity.clone(),
        qualifiers: [("generation".into(), "5".into())].into(),
    };
    let references = StatementReferences {
        query: Some(NativeDefinitionReference {
            native: identity.native(),
            definition_id: Some(identity.definition_id(&source).unwrap()),
        }),
        plan: Some(NativeDefinitionReference {
            native: plan_identity.native(),
            definition_id: Some(plan_identity.definition_id(&source).unwrap()),
        }),
    };
    let sql = "SELECT payload FROM public.orders WHERE customer_id=9264 AND note='fixture-private-literal'";
    let mut records = vec![
        TelemetryRecord::QueryDefinition(QueryDefinition {
            id: identity.definition_id(&source).unwrap(),
            identity,
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            context: [("native-option".into(), "enabled".into())].into(),
            context_available: true,
            variant_family: None,
            source_text_bytes: Some(sql.len().to_string()),
            sql: DefinitionPayload::sql(sql, source.target.engine()),
        }),
        TelemetryRecord::PlanDefinition(PlanDefinition {
            id: plan_identity.definition_id(&source).unwrap(),
            identity: plan_identity,
            query_definition_id: references.query.as_ref().unwrap().definition_id.clone(),
            engine_version: Some("18.1".into()),
            native_properties: [("native-plan-generation".into(), "5".into())].into(),
            source_text_bytes: None,
            plan: DefinitionPayload::plan(
                r#"{"Plan":{"Node Type":"Seq Scan","Relation Name":"orders"}}"#,
                source.target.engine(),
            ),
        }),
    ];
    for outcome in ["0", "3", "4"] {
        let mut duration = RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Microseconds, 1.25);
        duration.minimum = Some(0.0);
        duration.maximum = Some(3.0);
        duration.standard_deviation = Some(0.5);
        records.push(TelemetryRecord::QueryRuntime(RuntimeMeasurement {
            key: NativeMeasurementKey {
                native: NativeIdentity::new(format!("native-runtime-{outcome}"))
                    .qualified("execution_type", outcome)
                    .qualified("native-stats-generation", "7"),
                references: references.clone(),
            },
            executions: "9007199254740993".into(),
            metrics: vec![
                duration,
                RuntimeMetric::average(
                    RuntimeMetricKey::LogicalReads,
                    RuntimeMetricUnit::Pages { bytes_per_page: 16384 },
                    7.0,
                ),
                RuntimeMetric::average(
                    RuntimeMetricKey::TemporaryStorage,
                    RuntimeMetricUnit::Pages { bytes_per_page: 16384 },
                    1.25,
                ),
            ],
            first_execution_at: Some("2020-01-01T06:00:00.123456000Z".into()),
            last_execution_at: Some("2020-01-01T06:59:59.999999000Z".into()),
        }));
    }
    records.push(TelemetryRecord::QueryWaits(WaitMeasurement {
        key: NativeMeasurementKey {
            native: NativeIdentity::new("native-waits-1").qualified("execution_type", "0"),
            references,
        },
        category: "native-page-io".into(),
        unit: RuntimeMetricUnit::Microseconds,
        total: MeasurementValue::Integer("9007199254740993".into()),
        average: Some(1.25),
        minimum: Some(MeasurementValue::Integer("0".into())),
        maximum: None,
    }));
    (source, records)
}

/// The selected child retains routing metadata without a parent runtime or dispatcher payload.
pub fn query_variant_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (source, mut records) = query_evidence();
    let TelemetryRecord::QueryDefinition(query) = &mut records[0] else { panic!("query definition") };
    let parent = NativeQueryIdentity {
        query_id: "native-parent-6".into(),
        qualifiers: [("context".into(), "parent-context-1".into())].into(),
    };
    query.variant_family = Some(culprit_core::QueryVariantFamily {
        variant: query.identity.clone(),
        parent: parent.clone(),
        dispatcher: NativePlanIdentity {
            query: parent,
            plan_id: "native-dispatcher-9".into(),
            qualifiers: [("generation".into(), "2".into())].into(),
        },
    });
    assert!(query.is_valid());
    (source, records)
}

/// A synthetic source dictionary exercises the shared contract without copying any provider's native taxonomy.
pub fn wait_semantics(capture: WaitCaptureState) -> QueryWaitSemantics {
    QueryWaitSemantics {
        capture,
        coverage: WaitCaptureCoverage::Qualified,
        categories: [
            (
                "native-page-io".into(),
                NativeWaitMeaning {
                    native_name: "Native Page IO".into(),
                    disposition: WaitDisposition::Analyze(WaitClass::Io),
                },
            ),
            (
                "native-lock".into(),
                NativeWaitMeaning {
                    native_name: "Native Lock".into(),
                    disposition: WaitDisposition::Analyze(WaitClass::Lock),
                },
            ),
            (
                "native-idle".into(),
                NativeWaitMeaning { native_name: "Native Idle".into(), disposition: WaitDisposition::Ignore },
            ),
            (
                "native-unknown".into(),
                NativeWaitMeaning { native_name: "Native Unknown".into(), disposition: WaitDisposition::Unknown },
            ),
        ]
        .into(),
    }
}

pub fn query_wait_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let mut cases = [
        (Some(WaitCaptureState::Enabled), Some("native-page-io")),
        (Some(WaitCaptureState::Enabled), None),
        (None, Some("native-page-io")),
        (Some(WaitCaptureState::Enabled), Some("future-native-id")),
        (Some(WaitCaptureState::Enabled), Some("native-unknown")),
        (Some(WaitCaptureState::Enabled), Some("native-idle")),
        (Some(WaitCaptureState::Disabled), Some("native-page-io")),
        (Some(WaitCaptureState::Unavailable), Some("native-page-io")),
    ]
    .into_iter()
    .map(|(capture, category)| {
        let (mut source, mut records) = query_evidence();
        let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
        history.waits = capture.map(wait_semantics);
        records.retain_mut(|record| match record {
            TelemetryRecord::QueryWaits(wait) => match category {
                Some(category) => {
                    wait.category = category.into();
                    true
                }
                None => false,
            },
            _ => true,
        });
        (source, records)
    })
    .collect::<Vec<_>>();
    let (mut source, records) = cases[0].clone();
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
    history.waits.as_mut().unwrap().coverage = WaitCaptureCoverage::Unverified;
    cases.push((source, records));
    cases
}
