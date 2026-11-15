use super::fixture::Fixture;
use culprit_collector::{
    analytical::{
        evidence::{EvidenceFamily, EvidenceGapReason},
        test_support::{query_compilation, query_compilation_policy_evidence},
    },
    delivery::recovery::RecoveryRecords,
};
use culprit_core::{Engine, ErrorDetail, SourceOutcome, SourceState, telemetry::*};
use culprit_engine::collected_resources;

#[tokio::test]
async fn scan_enriched_compilation_policy_requires_full_authenticated_statement_not_warehouse_preview() {
    let (source, records) = query_compilation_policy_evidence();
    let expected = records[0].clone();
    let fixture = Fixture::new(source.clone(), records).await;
    let before = fixture.recovery_object_reads();
    let preview = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!preview.is_read_complete());
    let limited = collected_resources::compilation(&preview).unwrap();
    assert!(limited.report.entries.is_empty(), "a truncated warehouse SQL preview cannot establish the policy");
    assert_eq!(fixture.recovery_object_reads(), before);
    let capture = fixture.read_families_with_recovery(&[EvidenceFamily::OperationalSamples], Default::default()).await;
    assert!(capture.is_read_complete());
    assert_eq!(capture.metrics().recovery_objects, 1);
    assert_eq!(fixture.recovery_object_reads(), before + 1);
    let analysis = collected_resources::compilation(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.context.records[0].row.record, expected);
    let entry = &analysis.report.entries[0];
    assert!(entry.explanation.contains(&culprit_core::format::duration(6_000.0)));
    assert!(entry.investigation.contains("keep the hint if"));
    assert!(entry.metrics.iter().any(|metric| metric.is("view.compilation.metric.lifetime_duration")));
    assert!(analysis.report.limitations.contains(&culprit_core::t!("view.compilation.limit.policy")));
    let raw = serde_json::to_string(&analysis.context.records).unwrap();
    assert!(!raw.contains("private-compilation-value"));
    assert!(raw.contains("OPTION (RECOMPILE)"));
}

async fn recorded() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let archive = query_compilation("fixture:compilation", &digest(b"fixture schema"));
    let mut records = Vec::new();
    for batch in &archive.manifest.batches {
        let mut input = RecoveryRecords::open(&archive, batch).await.unwrap();
        while let Some(row) = input.next().await.unwrap() {
            records.push(row.record);
        }
    }
    (archive.manifest.source, records)
}

fn native(row: &mut TelemetryRecord) -> &mut MssqlQueryCompilation {
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::MssqlQueryCompilation(native),
        ..
    }) = row
    else {
        panic!("native query compilation")
    };
    native
}

fn rekey(row: &mut TelemetryRecord) {
    let key = native(row).key().unwrap();
    let TelemetryRecord::Operational(row) = row else { panic!() };
    row.key = key;
}

#[tokio::test]
async fn archived_compilations_reuse_the_scan_explanation_and_retain_exact_native_facts_once() {
    let (source, records) = recorded().await;
    let point = source.observation.snapshot().unwrap();
    let observations = records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = record else { panic!() };
            row.clone()
        })
        .collect::<Vec<_>>();
    let snapshot = CompilationSnapshot {
        source: point.sources[0].source.clone(),
        observed_from: point.observed_from.clone(),
        observed_until: point.observed_until.clone(),
        observations,
    };
    assert!(snapshot.is_valid(), "native compilation history precedes the independent point read");
    let source_outcome = SourceOutcome {
        source: snapshot.source.clone(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    };
    let expected =
        culprit_checks::compilation::explain(&snapshot.observations.iter().collect::<Vec<_>>(), Some(&source_outcome))
            .unwrap();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let reads_before = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let analysis = collected_resources::compilation(&capture).unwrap();
    assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.context.records.len(), records.len());
    assert_eq!(
        analysis.context.records[0].row,
        ExportedRecord::new(capture.source(), capture.capture(), records[0].clone()).unwrap()
    );
    assert!(
        analysis.report.entries[0]
            .metrics
            .iter()
            .any(|metric| metric.is("view.compilation.metric.count")
                && metric.text.as_deref() == Some("9007199254740993"))
    );
    assert_eq!(fixture.recovery_object_reads(), reads_before, "inline compilation facts need no native payload reread");
    assert_eq!(analysis.context.metrics.recovery_objects, 0);
    let value = serde_json::to_value(&analysis).unwrap();
    assert!(value.get("snapshot").is_none() && value.get("workload").is_none() && value.get("findings").is_none());
    assert!(value["report"]["entries"][0].get("total_memory").is_none());
}

#[tokio::test]
async fn reused_native_query_numbers_keep_distinct_compilation_incarnations_without_merging_counts() {
    let (mut source, mut records) = recorded().await;
    native(&mut records[0]).compilations = "2".into();
    let mut changed = records[0].clone();
    native(&mut changed).identity.qualifiers.insert("context_settings_id".into(), "6".into());
    native(&mut changed).identity.qualifiers.insert("initial_compile_at".into(), "2020-01-01T07:00:00Z".into());
    native(&mut changed).compilations = "5".into();
    rekey(&mut changed);
    records.push(changed);
    let SourceObservation::Snapshot(point) = &mut source.observation else { panic!() };
    point.records = 2;
    point.sources[0] = StateSourceCoverage::collected(point.sources[0].source.clone(), 2);
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let analysis = collected_resources::compilation(&capture).unwrap();
    assert_eq!(analysis.report.entries.len(), 2);
    assert!(analysis.report.entries.iter().all(|entry| entry.identity.query_id == "42"));
    let mut counts = analysis
        .report
        .entries
        .iter()
        .map(|entry| {
            entry
                .metrics
                .iter()
                .find(|metric| metric.is("view.compilation.metric.count"))
                .unwrap()
                .text
                .clone()
                .unwrap()
        })
        .collect::<Vec<_>>();
    counts.sort();
    assert_eq!(counts, ["2", "5"]);
    assert_ne!(analysis.report.entries[0].observation_key, analysis.report.entries[1].observation_key);
}

#[tokio::test]
async fn native_coverage_and_missing_warehouse_rows_remain_independent_compilation_qualifications() {
    for state in [SourceState::Partial, SourceState::Empty, SourceState::Unavailable, SourceState::NotApplicable] {
        let (mut source, mut rows) = recorded().await;
        let SourceObservation::Snapshot(point) = &mut source.observation else { panic!() };
        let name = point.sources[0].source.clone();
        if state != SourceState::Partial {
            rows.clear();
            point.records = 0;
        }
        point.sources[0] = match state {
            SourceState::Partial => StateSourceCoverage::observed(name, 1, false),
            SourceState::Empty => StateSourceCoverage::collected(name, 0),
            SourceState::Unavailable => {
                StateSourceCoverage::unavailable(name, culprit_core::ErrorCode::PermissionDenied, None)
            }
            SourceState::NotApplicable => StateSourceCoverage::not_applicable(name),
            _ => unreachable!(),
        };
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(capture.is_read_complete());
        let report = collected_resources::compilation(&capture).unwrap().report;
        assert_eq!(report.source_state, Some(state));
        assert_eq!(report.entries.len(), rows.len());
    }
    let (source, rows) = recorded().await;
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let analysis = collected_resources::compilation(&capture).unwrap();
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    assert!(analysis.report.entries.is_empty());
    assert!(analysis.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(analysis.context.source.observation.snapshot().unwrap().sources[0].state, SourceState::Collected);
}

#[tokio::test]
async fn compilation_admission_rejects_foreign_subjects_formats_datasets_and_operational_fact_kinds() {
    for shape in ["shared", "foreign-native-format", "dataset", "family", "fact"] {
        let (mut source, mut rows) = recorded().await;
        match shape {
            "shared" => {
                source.target = CollectionTarget::Shared(SharedResourceIdentity {
                    engine: Engine::Mssql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("namespace"),
                    native: NativeIdentity::new("instance"),
                })
            }
            "foreign-native-format" => {
                let CollectionTarget::Database(database) = &mut source.target else { panic!() };
                database.engine = Engine::Postgres;
            }
            "dataset" => {
                let SourceObservation::Snapshot(point) = &mut source.observation else { panic!() };
                point.dataset = SnapshotDataset::Operational;
            }
            "fact" => {
                let (_, mut other) = culprit_collector::analytical::test_support::memory_failure_evidence();
                let TelemetryRecord::Operational(row) = &mut other[0] else { panic!() };
                row.source = source.observation.snapshot().unwrap().sources[0].source.clone();
                rows = vec![other.remove(0)];
            }
            "family" => {}
            _ => unreachable!(),
        }
        let fixture = Fixture::new(source, rows).await;
        let family =
            if shape == "family" { EvidenceFamily::ResourceSamples } else { EvidenceFamily::OperationalSamples };
        let capture = fixture.read_families(&[family]).await;
        let error = collected_resources::compilation(&capture).unwrap_err();
        let problem = if shape == "fact" { DeliveryProblem::Integrity } else { DeliveryProblem::UnsupportedSchema };
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(problem)), "{shape}");
    }
}
