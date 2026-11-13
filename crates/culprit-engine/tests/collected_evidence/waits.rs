use super::*;
use culprit_collector::analytical::evidence::EvidenceFamily;
use culprit_core::collected_analysis::CollectedQueryAnalysis;

fn history() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (_, mut source, mut records) = recorded();
    records.truncate(2);
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
    history.waits = Some(QueryWaitSemantics {
        capture: WaitCaptureState::Enabled,
        coverage: WaitCaptureCoverage::Qualified,
        categories: WaitClass::ALL
            .iter()
            .map(|class| {
                (
                    format!("native-{}", class.key()),
                    NativeWaitMeaning {
                        native_name: format!("native-name-{}", class.key()),
                        disposition: WaitDisposition::Analyze(*class),
                    },
                )
            })
            .chain([
                (
                    "native-idle".into(),
                    NativeWaitMeaning { native_name: "native idle".into(), disposition: WaitDisposition::Ignore },
                ),
                (
                    "native-unknown".into(),
                    NativeWaitMeaning { native_name: "native unknown".into(), disposition: WaitDisposition::Unknown },
                ),
            ])
            .collect(),
    });
    let TelemetryRecord::QueryDefinition(query) = &mut records[0] else { panic!() };
    query.sql = DefinitionPayload::sql("SELECT * FROM dbo.Orders WHERE CustomerId=@id", source.target.engine());
    let TelemetryRecord::QueryRuntime(runtime) = &mut records[1] else { panic!() };
    runtime.executions = "100".into();
    runtime.metrics = vec![
        RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Milliseconds, 1000.0),
        RuntimeMetric::average(RuntimeMetricKey::CpuTime, RuntimeMetricUnit::Milliseconds, 100.0),
    ];
    runtime.first_execution_at = Some(history.interval.from.clone());
    runtime.last_execution_at = Some(history.interval.until.clone());
    (source, records)
}

fn dictionary(source: &mut CollectionDescription) -> &mut QueryWaitSemantics {
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
    history.waits.as_mut().unwrap()
}

fn native_wait(
    records: &[TelemetryRecord],
    category: &str,
    total: MeasurementValue,
    unit: RuntimeMetricUnit,
    id: &str,
) -> WaitMeasurement {
    let TelemetryRecord::QueryRuntime(runtime) = &records[1] else { panic!() };
    let mut key = runtime.key.clone();
    key.native.id = id.into();
    WaitMeasurement { key, category: category.into(), unit, total, average: None, minimum: None, maximum: None }
}

fn integer_wait(records: &[TelemetryRecord], category: &str, total: &str, id: &str) -> WaitMeasurement {
    native_wait(records, category, MeasurementValue::Integer(total.into()), RuntimeMetricUnit::Milliseconds, id)
}

fn query(analysis: &CollectedQueryAnalysis) -> &QueryStat {
    let queries = &analysis
        .workload
        .as_ref()
        .unwrap_or_else(|| {
            panic!(
                "query workload unavailable: limits={:?}; read gaps={:?}",
                analysis.limitations, analysis.context.evidence.read_gaps
            )
        })
        .queries;
    assert_eq!(queries.len(), 1);
    &queries[0]
}

fn has_limit(analysis: &CollectedQueryAnalysis, reason: CollectedAnalysisLimit) -> bool {
    analysis.limitations.iter().any(|limit| limit.reason == reason)
}

#[tokio::test]
async fn authenticated_dictionary_projects_every_class_and_keeps_native_names_and_totals() {
    let (source, mut records) = history();
    for (index, class) in WaitClass::ALL.iter().enumerate() {
        records.push(TelemetryRecord::QueryWaits(integer_wait(
            &records,
            &format!("native-{}", class.key()),
            &(100 + index).to_string(),
            &format!("wait-{index}"),
        )));
    }
    let waits = records.iter().filter(|row| matches!(row, TelemetryRecord::QueryWaits(_))).cloned().collect::<Vec<_>>();
    let fixture = Fixture::new(source.clone(), records).await;
    let capture = fixture.read().await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let analysis = collected_evidence::analyze(&capture).unwrap();
    let projected = query(&analysis);
    for (index, class) in WaitClass::ALL.iter().enumerate() {
        let expected = Some((100 + index) as f64);
        assert_eq!(projected.observed_wait_ms(*class), expected);
        assert_eq!(projected.measured_wait_ms(*class), expected);
    }
    assert!(projected.waits.iter().all(|row| row.wait_type.is_none() && row.tasks.is_none()));
    assert_eq!(projected.waits_coverage.as_ref().unwrap().classes.len(), WaitClass::ALL.len());
    assert_eq!(analysis.context.evidence.source, source);
    for wait in waits {
        assert!(analysis.context.evidence.records.iter().any(|record| record.row.record == wait));
    }
    assert!(!has_limit(&analysis, CollectedAnalysisLimit::MetricNotProjected));
    assert!(!has_limit(&analysis, CollectedAnalysisLimit::UnknownWaitCategory));
}

#[tokio::test]
async fn dictionary_disposition_is_authoritative_over_engine_source_native_id_and_display_name() {
    let (mut source, mut records) = history();
    let CollectionTarget::Database(database) = &mut source.target else { panic!() };
    database.engine = culprit_core::Engine::Postgres;
    source.source = "arbitrary-native-query-history".into();
    dictionary(&mut source).categories = [(
        "3".into(),
        NativeWaitMeaning { native_name: "Lock".into(), disposition: WaitDisposition::Analyze(WaitClass::Memory) },
    )]
    .into();
    let TelemetryRecord::QueryDefinition(definition) = &mut records[0] else { panic!() };
    definition.id = definition.identity.definition_id(&source).unwrap();
    let reference =
        NativeDefinitionReference { native: definition.identity.native(), definition_id: Some(definition.id.clone()) };
    let TelemetryRecord::QueryRuntime(runtime) = &mut records[1] else { panic!() };
    runtime.key.references.query = Some(reference);
    records.push(TelemetryRecord::QueryWaits(native_wait(
        &records,
        "3",
        MeasurementValue::Real(1.25),
        RuntimeMetricUnit::Seconds,
        "arbitrary-native-id",
    )));
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
    let projected = query(&analysis);
    assert_eq!(projected.measured_wait_ms(WaitClass::Memory), Some(1250.0));
    assert_eq!(projected.measured_wait_ms(WaitClass::Lock), None);
    assert_eq!(projected.measured_wait_ms(WaitClass::Other), None);
    assert_eq!(projected.waits.len(), 1);
    assert_eq!(projected.waits[0].category, "Memory");
}

#[tokio::test]
async fn a_published_present_empty_wait_batch_can_measure_zero_for_qualified_classes_only() {
    let (mut source, records) = history();
    dictionary(&mut source).categories.retain(|key, _| key == "native-lock" || key == "native-unknown");
    let fixture = Fixture::new(source, records).await;
    let empty =
        fixture.range.manifest().batches.iter().find(|batch| batch.stream == TelemetryStream::QueryWaits).unwrap();
    assert!(empty.delivery_required && empty.rows.is_empty());
    let capture = fixture.read().await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let analysis = collected_evidence::analyze(&capture).unwrap();
    let projected = query(&analysis);
    assert!(projected.waits.is_empty());
    assert_eq!(projected.observed_wait_ms(WaitClass::Lock), None);
    assert_eq!(projected.measured_wait_ms(WaitClass::Lock), Some(0.0));
    assert_eq!(projected.measured_wait_ms(WaitClass::Memory), None);
    assert!(!has_limit(&analysis, CollectedAnalysisLimit::UnknownWaitCategory));
    let lock = analysis.assessments.iter().find(|assessment| assessment.check == "lock-time").unwrap();
    assert_eq!(lock.status, CheckStatus::Assessed);
}

#[tokio::test]
async fn unselected_wait_family_is_unknown_even_when_the_requested_read_has_no_gaps() {
    let (source, records) = history();
    let fixture = Fixture::new(source, records).await;
    let capture = fixture
        .read_families(&[
            EvidenceFamily::QueryDefinitions,
            EvidenceFamily::PlanDefinitions,
            EvidenceFamily::QueryRuntime,
        ])
        .await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let analysis = collected_evidence::analyze(&capture).unwrap();
    let projected = query(&analysis);
    assert!(projected.waits.is_empty());
    assert_eq!(projected.measured_wait_ms(WaitClass::Lock), None);
    assert!(has_limit(&analysis, CollectedAnalysisLimit::WaitFamilyIncomplete));
    assert_ne!(
        analysis.assessments.iter().find(|assessment| assessment.check == "lock-time").unwrap().status,
        CheckStatus::Assessed
    );
}

#[tokio::test]
async fn currently_enabled_capture_preserves_positive_waits_without_proving_the_past_interval_complete() {
    let (mut source, mut records) = history();
    dictionary(&mut source).coverage = WaitCaptureCoverage::Unverified;
    let wait = integer_wait(&records, "native-io", "15000", "historical-positive");
    records.push(TelemetryRecord::QueryWaits(wait.clone()));
    let fixture = Fixture::new(source.clone(), records).await;
    let capture = fixture.read().await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let analysis = collected_evidence::analyze(&capture).unwrap();
    let projected = query(&analysis);
    assert_eq!(projected.observed_wait_ms(WaitClass::Io), Some(15000.0));
    assert_eq!(projected.measured_wait_ms(WaitClass::Io), None);
    assert_eq!(projected.measured_wait_ms(WaitClass::Lock), None);
    assert!(has_limit(&analysis, CollectedAnalysisLimit::WaitIntervalCoverageUnverified));
    assert_eq!(analysis.context.evidence.source, source);
    assert!(
        analysis
            .context
            .evidence
            .records
            .iter()
            .any(|record| record.row.record == TelemetryRecord::QueryWaits(wait.clone()))
    );
    assert_ne!(
        analysis.assessments.iter().find(|assessment| assessment.check == "lock-time").unwrap().status,
        CheckStatus::Assessed
    );
}

#[tokio::test]
async fn turning_capture_off_or_losing_current_config_does_not_erase_historical_positive_waits() {
    for (capture, reason) in [
        (WaitCaptureState::Disabled, CollectedAnalysisLimit::WaitCaptureDisabled),
        (WaitCaptureState::Unavailable, CollectedAnalysisLimit::WaitCaptureUnavailable),
    ] {
        let (mut source, mut records) = history();
        dictionary(&mut source).capture = capture;
        dictionary(&mut source).coverage = WaitCaptureCoverage::Unverified;
        let wait = integer_wait(&records, "native-lock", "60000", "retained-positive");
        records.push(TelemetryRecord::QueryWaits(wait.clone()));
        let fixture = Fixture::new(source, records).await;
        let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
        let projected = query(&analysis);
        assert_eq!(projected.observed_wait_ms(WaitClass::Lock), Some(60000.0));
        assert_eq!(projected.measured_wait_ms(WaitClass::Lock), None);
        assert!(has_limit(&analysis, reason));
        assert!(has_limit(&analysis, CollectedAnalysisLimit::WaitIntervalCoverageUnverified));
        assert!(
            analysis
                .context
                .evidence
                .records
                .iter()
                .any(|record| record.row.record == TelemetryRecord::QueryWaits(wait.clone()))
        );
        assert!(analysis.findings.iter().any(|finding| finding.check == "lock-time"));
    }
}

#[tokio::test]
async fn current_mode_does_not_override_independent_qualification_of_a_historical_interval() {
    for capture in [WaitCaptureState::Disabled, WaitCaptureState::Unavailable] {
        let (mut source, records) = history();
        dictionary(&mut source).capture = capture;
        let fixture = Fixture::new(source, records).await;
        let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
        assert_eq!(query(&analysis).measured_wait_ms(WaitClass::Lock), Some(0.0));
        assert!(!has_limit(&analysis, CollectedAnalysisLimit::WaitIntervalCoverageUnverified));
    }
}

#[tokio::test]
async fn a_missing_wait_row_preserves_known_positive_subtotals_without_inventing_absent_zeroes() {
    let (source, mut records) = history();
    records.push(TelemetryRecord::QueryWaits(integer_wait(&records, "native-lock", "60000", "lost-lock-row")));
    records.push(TelemetryRecord::QueryWaits(integer_wait(&records, "native-io", "15000", "retained-io-row")));
    let fixture = Fixture::new(source, records).await;
    fixture.remove_first_row(TelemetryStream::QueryWaits);
    let capture = fixture.read().await;
    assert!(!capture.is_read_complete());
    let analysis = collected_evidence::analyze(&capture).unwrap();
    let projected = query(&analysis);
    assert_eq!(projected.observed_wait_ms(WaitClass::Io), Some(15000.0));
    assert_eq!(projected.measured_wait_ms(WaitClass::Io), None);
    assert_eq!(projected.measured_wait_ms(WaitClass::Lock), None);
    assert!(has_limit(&analysis, CollectedAnalysisLimit::WaitFamilyIncomplete));
    assert!(analysis.findings.iter().all(|finding| finding.check != "lock-time"));
    assert_ne!(
        analysis.assessments.iter().find(|assessment| assessment.check == "lock-time").unwrap().status,
        CheckStatus::Assessed
    );
}

#[tokio::test]
async fn observed_unknown_categories_preserve_positive_locks_but_never_become_other() {
    for category in ["native-unknown", "future-unrecognized-id"] {
        let (source, mut records) = history();
        records.push(TelemetryRecord::QueryWaits(integer_wait(&records, "native-lock", "60000", "known-lock")));
        records.push(TelemetryRecord::QueryWaits(integer_wait(&records, category, "9000", "unknown-wait")));
        let fixture = Fixture::new(source, records).await;
        let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
        let projected = query(&analysis);
        assert_eq!(projected.observed_wait_ms(WaitClass::Lock), Some(60000.0));
        assert_eq!(projected.measured_wait_ms(WaitClass::Lock), None);
        assert_eq!(projected.observed_wait_ms(WaitClass::Other), None);
        assert_eq!(projected.measured_wait_ms(WaitClass::Other), None);
        assert!(has_limit(&analysis, CollectedAnalysisLimit::UnknownWaitCategory));
        assert!(analysis.findings.iter().any(|finding| finding.check == "lock-time"));
        assert_eq!(
            analysis
                .context
                .evidence
                .records
                .iter()
                .filter(|record| matches!(record.row.record, TelemetryRecord::QueryWaits(_)))
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn ignored_native_categories_do_not_poison_measurement_or_become_other() {
    let (source, mut records) = history();
    records.push(TelemetryRecord::QueryWaits(integer_wait(&records, "native-idle", "99999", "ignored")));
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
    let projected = query(&analysis);
    assert!(projected.waits.is_empty());
    assert_eq!(projected.measured_wait_ms(WaitClass::Lock), Some(0.0));
    assert_eq!(projected.measured_wait_ms(WaitClass::Other), Some(0.0));
    assert!(!has_limit(&analysis, CollectedAnalysisLimit::UnknownWaitCategory));
}

#[tokio::test]
async fn exact_qualifier_joins_do_not_attach_another_outcome_replica_or_plan() {
    for dimension in ["execution_type", "replica_group_id", "plan_id"] {
        let (source, mut records) = history();
        let TelemetryRecord::QueryRuntime(runtime) = &mut records[1] else { panic!() };
        runtime.key.native.qualifiers.insert(dimension.into(), "0".into());
        let mut wait = integer_wait(&records, "native-lock", "60000", "foreign-dimension");
        wait.key.native.qualifiers.insert(dimension.into(), "1".into());
        records.push(TelemetryRecord::QueryWaits(wait.clone()));
        let fixture = Fixture::new(source, records).await;
        let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
        let projected = query(&analysis);
        assert!(projected.waits.is_empty());
        assert_eq!(projected.measured_wait_ms(WaitClass::Lock), None);
        assert!(has_limit(&analysis, CollectedAnalysisLimit::WaitWithoutRuntime));
        assert!(
            analysis
                .context
                .evidence
                .records
                .iter()
                .any(|record| record.row.record == TelemetryRecord::QueryWaits(wait.clone()))
        );
    }
}

#[tokio::test]
async fn exact_plan_definition_references_isolate_waits_even_with_identical_sql_and_native_dimensions() {
    let (source, mut records) = history();
    let TelemetryRecord::QueryDefinition(definition) = &records[0] else { panic!() };
    let query_identity = definition.identity.clone();
    let query_id = definition.id.clone();
    let mut references = vec![];
    for id in ["first-native-plan", "second-native-plan"] {
        let identity = NativePlanIdentity {
            plan_id: id.into(),
            query: query_identity.clone(),
            qualifiers: [("native-plan-generation".into(), "1".into())].into(),
        };
        let definition_id = identity.definition_id(&source).unwrap();
        references
            .push(NativeDefinitionReference { native: identity.native(), definition_id: Some(definition_id.clone()) });
        records.push(TelemetryRecord::PlanDefinition(PlanDefinition {
            id: definition_id,
            identity,
            query_definition_id: Some(query_id.clone()),
            engine_version: None,
            native_properties: Default::default(),
            source_text_bytes: None,
            plan: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        }));
    }
    let TelemetryRecord::QueryRuntime(first) = &mut records[1] else { panic!() };
    first.key.references.plan = Some(references[0].clone());
    let mut second = first.clone();
    second.key.native.id = "second-native-runtime".into();
    second.key.references.plan = Some(references[1].clone());
    let wait = integer_wait(&records, "native-lock", "60000", "first-plan-lock");
    records.extend([TelemetryRecord::QueryRuntime(second), TelemetryRecord::QueryWaits(wait)]);
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
    let queries = &analysis.workload.as_ref().unwrap().queries;
    assert_eq!(queries.len(), 2);
    let by_plan = |id: &str| queries.iter().find(|query| query.plan_id.as_deref() == Some(id)).unwrap();
    assert_eq!(by_plan("first-native-plan").measured_wait_ms(WaitClass::Lock), Some(60000.0));
    assert_eq!(by_plan("second-native-plan").measured_wait_ms(WaitClass::Lock), Some(0.0));
    assert!(!has_limit(&analysis, CollectedAnalysisLimit::WaitWithoutRuntime));
}

#[tokio::test]
async fn native_time_units_and_multiple_category_fragments_fold_once_without_execution_multiplication() {
    let (mut source, mut records) = history();
    dictionary(&mut source).categories.insert(
        "another-native-lock".into(),
        NativeWaitMeaning {
            native_name: "other native lock".into(),
            disposition: WaitDisposition::Analyze(WaitClass::Lock),
        },
    );
    for (index, (unit, value)) in [
        (RuntimeMetricUnit::Picoseconds, 1_000_000_000),
        (RuntimeMetricUnit::Nanoseconds, 2_000_000),
        (RuntimeMetricUnit::Microseconds, 3_000),
        (RuntimeMetricUnit::Milliseconds, 4),
        (RuntimeMetricUnit::Seconds, 5),
    ]
    .into_iter()
    .enumerate()
    {
        let category = if index % 2 == 0 { "native-lock" } else { "another-native-lock" };
        records.push(TelemetryRecord::QueryWaits(native_wait(
            &records,
            category,
            MeasurementValue::Integer(value.to_string()),
            unit,
            &format!("unit-{index}"),
        )));
    }
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
    let projected = query(&analysis);
    assert_eq!(projected.waits.len(), 1);
    assert_eq!(projected.measured_wait_ms(WaitClass::Lock), Some(5010.0));
    assert_eq!(projected.executions, 100.0);
}

#[tokio::test]
async fn integer_precision_and_conversion_overflow_keep_raw_values_and_only_supported_lower_bounds() {
    for (values, expected) in
        [(vec!["9007199254740993"], None), (vec!["9007199254740991", "2"], Some(9007199254740991.0))]
    {
        let (source, mut records) = history();
        for (index, value) in values.iter().enumerate() {
            records.push(TelemetryRecord::QueryWaits(integer_wait(
                &records,
                "native-lock",
                value,
                &format!("exact-{index}"),
            )));
        }
        let fixture = Fixture::new(source, records).await;
        let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
        let projected = query(&analysis);
        assert_eq!(projected.observed_wait_ms(WaitClass::Lock), expected);
        assert_eq!(projected.measured_wait_ms(WaitClass::Lock), None);
        assert!(has_limit(&analysis, CollectedAnalysisLimit::CounterPrecision));
        assert_eq!(
            analysis
                .context
                .evidence
                .records
                .iter()
                .filter(|record| matches!(record.row.record, TelemetryRecord::QueryWaits(_)))
                .count(),
            values.len()
        );
    }
    let (source, mut records) = history();
    records.push(TelemetryRecord::QueryWaits(native_wait(
        &records,
        "native-lock",
        MeasurementValue::Real(f64::MAX),
        RuntimeMetricUnit::Seconds,
        "overflow",
    )));
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
    assert_eq!(query(&analysis).observed_wait_ms(WaitClass::Lock), None);
    assert_eq!(query(&analysis).measured_wait_ms(WaitClass::Lock), None);
    assert!(has_limit(&analysis, CollectedAnalysisLimit::CounterPrecision));
}

#[tokio::test]
async fn missing_publication_does_not_turn_an_empty_read_into_a_measured_workload() {
    let (source, records) = history();
    let fixture = Fixture::new(source, records).await;
    fixture.unpublish();
    let capture = fixture.read().await;
    assert!(!capture.is_read_complete());
    let analysis = collected_evidence::analyze(&capture).unwrap();
    assert!(analysis.workload.is_none());
    assert!(analysis.context.query_inputs.is_empty());
    assert!(
        analysis
            .assessments
            .iter()
            .filter(|assessment| assessment.check == "lock-time")
            .all(|assessment| assessment.status != CheckStatus::Assessed)
    );
}
