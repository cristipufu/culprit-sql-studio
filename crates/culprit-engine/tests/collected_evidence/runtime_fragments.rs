use super::fixture::Fixture;
use super::*;
use culprit_collector::analytical::{evidence::EvidenceFamily, test_support::query_runtime_fragment_evidence};
use culprit_core::collected_analysis::CollectedRuntimeTimeTotal;
#[tokio::test]
async fn unknown_duration_mean_keeps_exact_population_executions_and_original_endpoints_but_no_time_denominator() {
    let (source, records) = query_runtime_fragment_evidence(true);
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let analysis = culprit_engine::collected_evidence::analyze(&capture, None).unwrap();
    let result = &analysis.analyses[0];
    let totals = result.runtime_totals.as_ref().unwrap();
    assert_eq!(totals.executions, "5");
    assert_eq!(totals.query_identities, "1");
    assert!(matches!(totals.duration, CollectedRuntimeTimeTotal::Incomplete));
    assert!(matches!(totals.cpu, CollectedRuntimeTimeTotal::Incomplete));
    assert!(result.workload.is_none());
    assert!(
        result.limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::RuntimeDenominatorUnavailable)
    );
    let runtime = fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
    let report = culprit_engine::collected_resources::query_runtime_fragments(&runtime).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.entries[0].executions, "2");
    assert_eq!(report.entries[1].executions, "3");
    assert_eq!(
        report.entries[0].metrics[0].statistics[0].text.as_deref(),
        Some(culprit_core::t!("view.runtime_fragments.unknown").as_str())
    );
    assert!(report.entries[0].metrics[0].statistics[1].text.as_deref().unwrap().contains("9007199254740993"));
    assert_eq!(report.entries[1].native.native.qualifiers["execution_type"], "3");
}
#[tokio::test]
async fn unrepresentable_native_count_keeps_exact_count_and_native_extrema_without_a_rounded_workload() {
    let (source, mut records) = query_runtime_fragment_evidence(false);
    let mut first = true;
    for record in &mut records {
        if let TelemetryRecord::QueryRuntime(runtime) = record {
            runtime.executions = if first { "9007199254740993".into() } else { "0".into() };
            first = false;
        }
    }
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let report = culprit_engine::collected_evidence::analyze(&capture, None).unwrap();
    let result = &report.analyses[0];
    let totals = result.runtime_totals.as_ref().unwrap();
    assert_eq!(totals.executions, "9007199254740993");
    assert!(matches!(totals.duration, CollectedRuntimeTimeTotal::Incomplete));
    assert!(result.workload.is_none());
    assert!(report.context.records.iter().any(|record|matches!(&record.row.record,TelemetryRecord::QueryRuntime(row) if row.metric(&RuntimeMetricKey::Duration).is_some_and(|metric|metric.last==Some(MeasurementValue::Integer("9007199254740993".into()))))));
}
#[tokio::test]
async fn complete_native_means_keep_existing_workload_totals_while_missing_archive_rows_withhold_population_counts() {
    let (source, records) = query_runtime_fragment_evidence(false);
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let analysis = culprit_engine::collected_evidence::analyze(&capture, None).unwrap();
    let result = &analysis.analyses[0];
    let totals = result.runtime_totals.as_ref().unwrap();
    assert_eq!(totals.executions, "5");
    assert!(matches!(totals.duration,CollectedRuntimeTimeTotal::Complete {total_ms} if total_ms==5.0));
    assert_eq!(result.workload.as_ref().unwrap().total_executions, 5.0);
    assert_eq!(result.workload.as_ref().unwrap().total_time_ms, 5.0);
    fixture.remove_first_runtime();
    let partial = fixture.read().await;
    let result = culprit_engine::collected_evidence::analyze(&partial, None).unwrap();
    assert!(result.analyses[0].runtime_totals.is_none());
    assert!(result.analyses[0].workload.is_none());
}
