use super::*;
use culprit_core::{Actionability, StatementTemporaryWork, collected_analysis::CollectedQueryAnalysis};

fn history() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (_, mut source, mut records) = recorded();
    records.truncate(2);
    let engine = culprit_core::Engine::Postgres;
    let CollectionTarget::Database(database) = &mut source.target else { panic!() };
    database.engine = engine;
    source.source = "embedding-native-runtime-history".into();
    let interval = source.query_history().unwrap().interval.clone();
    let TelemetryRecord::QueryDefinition(definition) = &mut records[0] else { panic!() };
    definition.identity.query_id = "native-space-statement".into();
    definition.id = definition.identity.definition_id(&source).unwrap();
    let sql = "SELECT value FROM readings WHERE id = $1";
    definition.sql = DefinitionPayload::sql(sql, engine);
    definition.source_text_bytes = Some(sql.len().to_string());
    let reference =
        NativeDefinitionReference { native: definition.identity.native(), definition_id: Some(definition.id.clone()) };
    let first = runtime(&mut records, 1);
    first.key.references.query = Some(reference);
    first.key.native.id = "native-space-fragment-0".into();
    first.key.native.qualifiers.insert("native-stats-epoch".into(), "epoch-a".into());
    first.executions = "2".into();
    first.first_execution_at = Some(interval.from);
    first.last_execution_at = Some(interval.until);
    first.metrics = vec![
        RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Milliseconds, 1000.0),
        RuntimeMetric::average(RuntimeMetricKey::CpuTime, RuntimeMetricUnit::Milliseconds, 50.0),
        RuntimeMetric::average(
            RuntimeMetricKey::TemporaryStorage,
            RuntimeMetricUnit::Pages { bytes_per_page: 16_384 },
            1.25,
        ),
    ];
    let mut second = first.clone();
    second.key.native.id = "native-space-fragment-1".into();
    second.executions = "10".into();
    second.metrics[0].average = 4000.0;
    second.metrics[2].average = 0.5;
    records.push(TelemetryRecord::QueryRuntime(second));
    (source, records)
}

fn runtime(records: &mut [TelemetryRecord], index: usize) -> &mut RuntimeMeasurement {
    let TelemetryRecord::QueryRuntime(row) = &mut records[index] else { panic!() };
    row
}

fn queries(analysis: &CollectedQueryAnalysis) -> &[QueryStat] {
    &analysis.workload.as_ref().unwrap_or_else(|| panic!("{:?}", analysis.limitations)).queries
}

fn has_limit(analysis: &CollectedQueryAnalysis, reason: CollectedAnalysisLimit) -> bool {
    analysis.limitations.iter().any(|limit| limit.reason == reason)
}

#[tokio::test]
async fn authenticated_runtime_space_averages_reach_the_same_pure_information_view_without_a_provider_switch() {
    let (source, records) = history();
    let expected_rows = records.clone();
    let fixture = Fixture::new(source.clone(), records).await;
    let capture = fixture.read().await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let analysis = collected_evidence::analyze(&capture).unwrap();
    let projected = &queries(&analysis)[0];
    assert_eq!(projected.executions, 12.0);
    assert_eq!(projected.total_time_ms, 42_000.0);
    assert_eq!(
        projected.temporary_work,
        Some(StatementTemporaryWork::SpaceUsage {
            source: source.source.clone(),
            bytes_per_page: 16_384,
            usage_pages_total: 7.5,
        })
    );
    let work = projected.temporary_work.as_ref().unwrap();
    let explanation = culprit_checks::temporary_work::explain(culprit_checks::temporary_work::evidence(work).unwrap());
    let finding = analysis.findings.iter().find(|finding| finding.check == "temp-spill").unwrap();
    assert_eq!(finding.actionability, Actionability::Information);
    assert_eq!(finding.summary, explanation.summary);
    assert_eq!(serde_json::to_value(&finding.metrics).unwrap(), serde_json::to_value(explanation.metrics).unwrap());
    assert!(finding.impact.db_time_share.is_none());
    assert!(finding.fix.as_ref().unwrap().sql.is_none());
    assert!(finding.queries.iter().all(|query| query.plan_node.is_none()));
    assert_eq!(analysis.context.evidence.source, source);
    assert_eq!(analysis.context.evidence.source.continuity, SourceContinuity::Unverified);
    assert!(!has_limit(&analysis, CollectedAnalysisLimit::MetricNotProjected));
    for row in expected_rows {
        let expected = ExportedRecord::new(capture.source(), capture.capture(), row).unwrap();
        assert!(analysis.context.evidence.records.iter().any(|record| record.row == expected));
    }
}

#[tokio::test]
async fn native_zero_is_retained_while_missing_or_partially_measured_fragments_remain_unknown() {
    for kind in ["zero", "missing", "partial"] {
        let (source, mut records) = history();
        for index in [1, 2] {
            let row = runtime(&mut records, index);
            if kind == "zero" {
                row.metrics[2].average = 0.0;
            } else if kind == "missing" || index == 1 {
                row.metrics.pop();
            }
        }
        let fixture = Fixture::new(source, records).await;
        let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
        let work = &queries(&analysis)[0].temporary_work;
        if kind == "zero" {
            assert!(matches!(work, Some(StatementTemporaryWork::SpaceUsage { usage_pages_total: 0.0, .. })));
        } else {
            assert!(work.is_none());
        }
        assert!(analysis.findings.iter().all(|finding| finding.check != "temp-spill"));
        assert_eq!(has_limit(&analysis, CollectedAnalysisLimit::MetricNotProjected), kind == "partial");
        assert_eq!(analysis.context.evidence.records.len(), 3);
    }
}

#[tokio::test]
async fn byte_units_page_conflicts_peak_metrics_and_opaque_native_lookalikes_do_not_acquire_space_meaning() {
    for kind in ["bytes", "page-size", "peak", "native-name", "overflow"] {
        let (source, mut records) = history();
        let row = runtime(&mut records, 1);
        match kind {
            "bytes" => row.metrics[2].unit = RuntimeMetricUnit::Bytes,
            "page-size" => row.metrics[2].unit = RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
            "peak" => row.metrics[2].key = RuntimeMetricKey::PeakMemory,
            "native-name" => row.metrics[2].key = RuntimeMetricKey::Native("mssql.avg_tempdb_space_used".into()),
            "overflow" => row.metrics[2].average = f64::MAX,
            _ => unreachable!(),
        }
        let fixture = Fixture::new(source, records).await;
        let capture = fixture.read().await;
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        let analysis = collected_evidence::analyze(&capture).unwrap();
        assert!(queries(&analysis)[0].temporary_work.is_none(), "{kind}");
        assert!(has_limit(&analysis, CollectedAnalysisLimit::MetricNotProjected), "{kind}");
        assert!(analysis.findings.iter().all(|finding| finding.check != "temp-spill"));
        assert_eq!(analysis.context.evidence.records.len(), 3);
    }
}

#[tokio::test]
async fn native_epochs_and_outcomes_are_separate_populations_without_reset_or_rate_inference() {
    for dimension in ["native-stats-epoch", "execution_type"] {
        let (source, mut records) = history();
        runtime(&mut records, 2).key.native.qualifiers.insert(dimension.into(), "different-native-population".into());
        let fixture = Fixture::new(source.clone(), records).await;
        let analysis = collected_evidence::analyze(&fixture.read().await).unwrap();
        let mut amounts = queries(&analysis)
            .iter()
            .map(|query| match query.temporary_work.as_ref().unwrap() {
                StatementTemporaryWork::SpaceUsage { usage_pages_total, .. } => *usage_pages_total,
                _ => panic!(),
            })
            .collect::<Vec<_>>();
        amounts.sort_by(f64::total_cmp);
        assert_eq!(amounts, vec![2.5, 5.0]);
        assert!(analysis.context.query_inputs.iter().all(|input| input.native_rows.len() == 1));
        assert_eq!(analysis.context.evidence.source, source);
        assert!(analysis.workload.as_ref().unwrap().counter_set.is_none());
        assert!(analysis.workload.as_ref().unwrap().history_inputs.is_empty());
    }
}

#[tokio::test]
async fn missing_rows_unpublished_captures_and_out_of_bounds_runtime_cannot_establish_complete_space_use() {
    for kind in ["missing-row", "unpublished", "outside-window"] {
        let (source, mut records) = history();
        if kind == "outside-window" {
            runtime(&mut records, 1).first_execution_at = Some("1900-01-01T00:00:00.000000000Z".into());
        }
        let fixture = Fixture::new(source, records).await;
        if kind == "missing-row" {
            fixture.remove_first_runtime();
        } else if kind == "unpublished" {
            fixture.unpublish();
        }
        let capture = fixture.read().await;
        let analysis = collected_evidence::analyze(&capture).unwrap();
        assert!(analysis.workload.is_none(), "{kind}");
        assert!(analysis.findings.iter().all(|finding| finding.check != "temp-spill"));
        if kind == "outside-window" {
            assert!(capture.is_read_complete());
            assert!(has_limit(&analysis, CollectedAnalysisLimit::RuntimeBounds));
        } else {
            assert!(!capture.is_read_complete());
        }
    }
}

#[tokio::test]
async fn execution_precision_limits_preserve_the_native_row_without_a_rounded_space_projection() {
    let (source, mut records) = history();
    runtime(&mut records, 1).executions = "9007199254740993".into();
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let analysis = collected_evidence::analyze(&capture).unwrap();
    assert!(analysis.workload.is_none());
    assert!(has_limit(&analysis, CollectedAnalysisLimit::CounterPrecision));
    assert!(analysis.context.evidence.records.iter().any(|record| {
        matches!(&record.row.record, TelemetryRecord::QueryRuntime(row) if row.executions == "9007199254740993")
    }));
}
