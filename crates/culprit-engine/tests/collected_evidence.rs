use culprit_core::{
    Capability, CheckStatus, QueryStat, Snapshot, collected_analysis::CollectedAnalysisLimit, telemetry::*,
};
use culprit_engine::collected_evidence;

mod fixture;
mod waits;
use fixture::Fixture;

fn recorded() -> (Snapshot, CollectionDescription, Vec<TelemetryRecord>) {
    let scan: Snapshot = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.json")).unwrap();
    let workload = scan.workload.as_ref().unwrap();
    let source = CollectionDescription {
        target: scan.database_identity().into(),
        source: culprit_core::source_id::QUERY_STORE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::QueryHistory(QueryHistoryDescription {
            waits: None,
            interval: TelemetryInterval {
                native_id: "recorded-interval".into(),
                from: workload.window_start.clone().unwrap(),
                until: workload.window_end.clone().unwrap(),
            },
            dimensions: Default::default(),
            native_properties: Default::default(),
        }),
    };
    let mut records = vec![];
    for query in &workload.queries[..2] {
        let identity = NativeQueryIdentity {
            query_id: query.id.clone(),
            qualifiers: [("native-generation".into(), "recorded-compile".into())].into(),
        };
        let id = identity.definition_id(&source).unwrap();
        records.push(TelemetryRecord::QueryDefinition(QueryDefinition {
            id: id.clone(),
            identity: identity.clone(),
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            context: Default::default(),
            context_available: false,
            source_text_bytes: Some(query.text.len().to_string()),
            sql: DefinitionPayload::sql(&query.text, source.target.engine()),
        }));
        let mut duration =
            RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Milliseconds, query.mean_time_ms);
        duration.maximum = query.max_time_ms;
        duration.standard_deviation = query.stddev_time_ms;
        let mut metrics = vec![duration];
        if let Some(cpu) = query.cpu_time_ms {
            metrics.push(RuntimeMetric::average(
                RuntimeMetricKey::CpuTime,
                RuntimeMetricUnit::Milliseconds,
                cpu / query.executions,
            ));
        }
        if let Some(rows) = query.rows {
            metrics.push(RuntimeMetric::average(
                RuntimeMetricKey::Rows,
                RuntimeMetricUnit::Count,
                rows / query.executions,
            ));
        }
        records.push(TelemetryRecord::QueryRuntime(RuntimeMeasurement {
            key: NativeMeasurementKey {
                native: NativeIdentity::new(format!("recorded-{}", query.id)).qualified("execution_type", "0"),
                references: StatementReferences {
                    query: Some(NativeDefinitionReference { native: identity.native(), definition_id: Some(id) }),
                    plan: None,
                },
            },
            executions: format!("{:.0}", query.executions),
            metrics,
            first_execution_at: query.first_seen.as_deref().map(canonical_time).transpose().unwrap(),
            last_execution_at: query.last_seen.as_deref().map(canonical_time).transpose().unwrap(),
        }));
    }
    (scan, source, records)
}

fn asserted_metrics(projected: &QueryStat, recorded: &QueryStat) {
    assert_eq!(projected.executions, recorded.executions);
    assert!((projected.mean_time_ms - recorded.mean_time_ms).abs() < 1e-10);
    assert!((projected.total_time_ms - recorded.mean_time_ms * recorded.executions).abs() < 1e-8);
    assert_eq!(projected.max_time_ms, recorded.max_time_ms);
    assert_eq!(projected.stddev_time_ms, recorded.stddev_time_ms);
}

#[tokio::test]
async fn authenticated_archived_scan_measurements_reach_the_existing_pure_checks_without_credentials() {
    let (scan, source, records) = recorded();
    let fixture = Fixture::new(source.clone(), records).await;
    let capture = fixture.read().await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let analysis = collected_evidence::analyze(&capture).unwrap();
    let workload = analysis.workload.as_ref().unwrap();
    assert_eq!(workload.kind, None);
    assert!(workload.windowed && workload.counter_set.is_none() && workload.history_inputs.is_empty());
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.context.manifest_sha256, fixture.range.manifest_digest());
    assert_eq!(analysis.provenance.as_ref().unwrap().origin, culprit_core::AnalysisOrigin::ArchivedTelemetry);
    let original = scan.workload.as_ref().unwrap();
    for input in &analysis.context.query_inputs {
        let native = &input.references.query.as_ref().unwrap().native.id;
        let recorded = original.queries.iter().find(|query| &query.id == native).unwrap();
        let projected = workload.queries.iter().find(|query| query.id == input.query_id).unwrap();
        asserted_metrics(projected, recorded);
        assert!(projected.logical_reads.is_none());
    }
    let finding = analysis.findings.iter().find(|finding| finding.check == "heavy-query").unwrap();
    assert!(!finding.summary.is_empty());
    assert!(!finding.verify.is_empty());
    assert!(finding.fix.as_ref().is_some_and(|fix| !fix.summary.is_empty()));
    assert!(analysis.locked.iter().any(|locked| locked.missing.contains(&Capability::TableStats)));
    assert!(
        analysis
            .assessments
            .iter()
            .any(|assessment| assessment.check == "heavy-query" && assessment.status == CheckStatus::Findings)
    );
    let encoded = serde_json::to_value(&analysis).unwrap();
    assert!(encoded.get("snapshot").is_none() && encoded.get("summary").is_none() && encoded.get("server").is_none());
}

#[tokio::test]
async fn missing_or_native_restricted_definitions_keep_positive_evidence_partial_without_a_clean_bill() {
    for restricted in [false, true] {
        let (_, source, mut records) = recorded();
        if restricted {
            let TelemetryRecord::QueryDefinition(query) = &mut records[0] else { panic!() };
            query.sql = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
        }
        let fixture = Fixture::new(source, records).await;
        if !restricted {
            fixture.remove_family(TelemetryStream::QueryDefinitions);
        }
        let capture = fixture.read().await;
        assert_eq!(capture.is_read_complete(), restricted);
        let analysis = collected_evidence::analyze(&capture).unwrap();
        if restricted {
            let workload = analysis.workload.as_ref().unwrap();
            assert_eq!(workload.queries.len(), 1);
            assert!(workload.total_time_ms > workload.queries.iter().map(|query| query.total_time_ms).sum::<f64>());
            let assessed = analysis.assessments.iter().find(|assessment| assessment.check == "heavy-query").unwrap();
            assert!(assessed.reason.is_some());
            assert!(
                analysis.limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::QueryTextUnavailable)
            );
        } else {
            assert!(analysis.workload.is_none());
            assert!(
                analysis
                    .assessments
                    .iter()
                    .filter(|assessment| assessment.check == "heavy-query")
                    .all(|assessment| assessment.status == CheckStatus::Unavailable)
            );
            assert!(
                analysis.limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::MissingQueryDefinition)
            );
        }
        assert!(
            analysis.context.records.iter().any(|record| matches!(record.row.record, TelemetryRecord::QueryRuntime(_)))
        );
    }
}

#[tokio::test]
async fn a_missing_runtime_fragment_cannot_inflate_the_remaining_querys_share() {
    let (_, source, records) = recorded();
    let fixture = Fixture::new(source, records).await;
    fixture.remove_first_runtime();
    let capture = fixture.read().await;
    assert!(capture.records().iter().any(|record| matches!(record.row().record, TelemetryRecord::QueryRuntime(_))));
    let analysis = collected_evidence::analyze(&capture).unwrap();
    assert!(analysis.workload.is_none());
    assert!(analysis.findings.iter().all(|finding| finding.check != "heavy-query"));
    assert!(
        analysis.limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::RuntimeDenominatorUnavailable)
    );
}

#[tokio::test]
async fn application_totals_exclude_proven_monitoring_but_keep_unknown_and_missing_definition_costs() {
    let (_, source, original) = recorded();
    let TelemetryRecord::QueryDefinition(definition) = &original[0] else { panic!() };
    let TelemetryRecord::QueryRuntime(runtime) = &original[1] else { panic!() };
    let mut records = vec![];
    for (index, (origin, executions, duration_ms, cpu_ms)) in [
        (Some(StatementOrigin::Application), 2, 10.0, 3.0),
        (Some(StatementOrigin::Culprit), 100, 1000.0, 500.0),
        (Some(StatementOrigin::CulpritMonitoring), 200, 2000.0, 1000.0),
        (Some(StatementOrigin::EngineInternal), 300, 3000.0, 1500.0),
        (Some(StatementOrigin::Unknown), 5, 20.0, 4.0),
        (None, 7, 30.0, 6.0),
    ]
    .into_iter()
    .enumerate()
    {
        let mut definition = definition.clone();
        definition.identity.query_id = format!("native-origin-{index}");
        definition.id = definition.identity.definition_id(&source).unwrap();
        let mut runtime = runtime.clone();
        runtime.key.native.id = format!("native-origin-runtime-{index}");
        runtime.key.references.query = Some(NativeDefinitionReference {
            native: definition.identity.native(),
            definition_id: Some(definition.id.clone()),
        });
        runtime.executions = executions.to_string();
        runtime.metrics = vec![
            RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Milliseconds, duration_ms),
            RuntimeMetric::average(RuntimeMetricKey::CpuTime, RuntimeMetricUnit::Milliseconds, cpu_ms),
        ];
        if let Some(origin) = origin {
            definition.origin = origin;
            records.push(TelemetryRecord::QueryDefinition(definition));
        }
        records.push(TelemetryRecord::QueryRuntime(runtime));
    }
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
    let workload = analysis.workload.as_ref().unwrap();
    assert_eq!(workload.queries.len(), 1);
    assert_eq!(workload.query_count, 3.0);
    assert_eq!(workload.total_executions, 14.0);
    assert_eq!(workload.total_time_ms, 330.0);
    assert_eq!(workload.total_cpu_ms, Some(68.0));
    assert_eq!(workload.queries[0].total_time_ms, 20.0);
    assert!(analysis.limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::UnknownStatementOrigin));
    assert!(analysis.limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::MissingQueryDefinition));
    assert_eq!(
        analysis
            .context
            .records
            .iter()
            .filter(|record| matches!(record.row.record, TelemetryRecord::QueryRuntime(_)))
            .count(),
        6
    );
}

#[tokio::test]
async fn opaque_native_wait_codes_remain_exact_without_becoming_semantic_other_waits() {
    let (_, source, mut records) = recorded();
    records.truncate(2);
    let TelemetryRecord::QueryRuntime(runtime) = &records[1] else { panic!() };
    let mut key = runtime.key.clone();
    key.native.id = "native-lock-category-wait".into();
    let wait = WaitMeasurement {
        key,
        category: "3".into(),
        unit: RuntimeMetricUnit::Milliseconds,
        total: MeasurementValue::Integer("999".into()),
        average: Some(499.5),
        minimum: Some(MeasurementValue::Integer("0".into())),
        maximum: None,
    };
    records.push(TelemetryRecord::QueryWaits(wait.clone()));
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
    assert!(analysis.workload.as_ref().unwrap().queries[0].waits.is_empty());
    assert!(
        analysis.context.records.iter().any(|record| record.row.record == TelemetryRecord::QueryWaits(wait.clone()))
    );
    assert!(analysis.limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::WaitSemanticsUnavailable));
    assert!(analysis.assessments.iter().find(|assessment| assessment.check == "heavy-query").unwrap().reason.is_some());
}

#[tokio::test]
async fn a_competing_native_generation_cannot_be_joined_by_query_number_or_display_sql() {
    let (_, source, mut records) = recorded();
    let TelemetryRecord::QueryRuntime(runtime) = &mut records[1] else { panic!() };
    runtime
        .key
        .references
        .query
        .as_mut()
        .unwrap()
        .native
        .qualifiers
        .insert("native-generation".into(), "recycled".into());
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    assert!(capture.is_read_complete());
    let error = collected_evidence::analyze(&capture).unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}

#[tokio::test]
async fn a_plan_from_another_query_generation_cannot_be_attached_through_matching_display_sql() {
    let (source, mut records) = culprit_collector::analytical::test_support::query_evidence();
    let TelemetryRecord::QueryDefinition(original) = &records[0] else { panic!() };
    let mut next = original.clone();
    next.identity.qualifiers.insert("userid".into(), "18".into());
    next.id = next.identity.definition_id(&source).unwrap();
    for record in &mut records {
        if let TelemetryRecord::QueryRuntime(row) = record {
            row.executions = "10".into();
        }
    }
    let TelemetryRecord::QueryRuntime(runtime) = &mut records[2] else { panic!() };
    runtime.key.references.query =
        Some(NativeDefinitionReference { native: next.identity.native(), definition_id: Some(next.id.clone()) });
    records.push(TelemetryRecord::QueryDefinition(next));
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    assert!(capture.is_read_complete());
    assert_eq!(
        collected_evidence::analyze(&capture).unwrap_err().detail,
        Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity))
    );
}

#[tokio::test]
async fn interval_shape_and_a_recorded_plan_version_do_not_invent_source_or_current_server_facts() {
    let (source, mut records) = culprit_collector::analytical::test_support::query_evidence();
    for record in &mut records {
        if let TelemetryRecord::QueryRuntime(row) = record {
            row.executions = "10".into();
        }
    }
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
    assert_eq!(analysis.workload.as_ref().unwrap().kind, None);
    assert!(
        analysis.limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::MissingCurrentServerMetadata)
    );
    assert!(analysis.context.records.iter().any(|record| matches!(&record.row.record,
        TelemetryRecord::PlanDefinition(plan) if plan.engine_version.as_deref() == Some("18.1"))));
    assert!(analysis.context.query_inputs.iter().any(|input| input.native_rows[0].qualifiers["execution_type"] == "3"));
    assert!(analysis.workload.as_ref().unwrap().queries.iter().all(|query| query.logical_reads.is_none()));
    assert!(analysis.limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::MetricNotProjected));
}

#[tokio::test]
async fn fragments_are_execution_weighted_and_different_outcomes_stay_separate() {
    let (_, source, mut records) = recorded();
    records.truncate(2);
    let TelemetryRecord::QueryRuntime(runtime) = &mut records[1] else { panic!() };
    runtime.executions = "1".into();
    runtime.metrics = vec![RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Seconds, 0.1)];
    let mut second = runtime.clone();
    second.key.native.id = "another-native-fragment".into();
    second.executions = "9".into();
    second.metrics[0].average = 1.0;
    let mut aborted = second.clone();
    aborted.key.native.id = "another-native-outcome".into();
    aborted.key.native.qualifiers.insert("execution_type".into(), "3".into());
    records.extend([TelemetryRecord::QueryRuntime(second), TelemetryRecord::QueryRuntime(aborted)]);
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
    let workload = analysis.workload.as_ref().unwrap();
    assert_eq!(workload.queries.len(), 2);
    assert_eq!(workload.query_count, 1.0);
    let input = analysis.context.query_inputs.iter().find(|input| input.native_rows.len() == 2).unwrap();
    let query = workload.queries.iter().find(|query| query.id == input.query_id).unwrap();
    assert_eq!(query.executions, 10.0);
    assert_eq!(query.total_time_ms, 9100.0);
    assert_eq!(query.mean_time_ms, 910.0);
    assert!(analysis.context.query_inputs.iter().any(|input| input.native_rows[0].qualifiers["execution_type"] == "3"));
}

#[tokio::test]
async fn native_large_counts_and_unknown_units_are_retained_without_rounded_analysis_or_zeroes() {
    let (_, source, mut records) = recorded();
    records.truncate(2);
    let TelemetryRecord::QueryRuntime(runtime) = &mut records[1] else { panic!() };
    runtime.executions = "9007199254740993".into();
    runtime.metrics.push(RuntimeMetric::average(
        RuntimeMetricKey::LogicalReads,
        RuntimeMetricUnit::Pages { bytes_per_page: 16384 },
        7.0,
    ));
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let analysis = collected_evidence::analyze(&capture).unwrap();
    assert!(analysis.workload.is_none());
    assert!(analysis.limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::CounterPrecision));
    let native = analysis
        .context
        .records
        .iter()
        .find_map(|record| match &record.row.record {
            TelemetryRecord::QueryRuntime(row) => Some(row),
            _ => None,
        })
        .unwrap();
    assert_eq!(native.executions, "9007199254740993");
    assert_eq!(
        native.metric(&RuntimeMetricKey::LogicalReads).unwrap().unit,
        RuntimeMetricUnit::Pages { bytes_per_page: 16384 }
    );
}
