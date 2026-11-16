use super::*;
use culprit_collector::analytical::test_support::compilation_interval_evidence;
use culprit_core::{
    BaselinePurpose, CompilationComparisonIssue, Snapshot, SourceOutcome, SourceState,
    telemetry::{CompilationSnapshot, NativeIdentity, NativeReadContext, TelemetryRecord},
};

fn snapshot(point: usize) -> Snapshot {
    let (source, records) = compilation_interval_evidence().into_iter().nth(point).unwrap();
    let context = NativeReadContext::from_snapshot(&source).unwrap();
    let database = context.target.database().unwrap().clone();
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.server.database = database.database.clone();
    snapshot.identity = Some(database);
    snapshot.id = ["20200101T120001.000Z", "20200101T120101.000Z"][point].into();
    snapshot.taken_at = context.observed_until.clone();
    snapshot.compilations_before = None;
    snapshot.options.as_mut().unwrap().workload_population = None;
    let observations = records
        .into_iter()
        .map(|record| match record {
            TelemetryRecord::Operational(row) => row,
            _ => panic!("native compilation fixture"),
        })
        .collect();
    let source_name = source.observation.snapshot().unwrap().sources[0].source.clone();
    snapshot.sources.push(SourceOutcome {
        source: source_name.clone(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    });
    snapshot.operations.query_compilation = Some(CompilationSnapshot {
        source: source_name,
        observed_from: source.observation.snapshot().unwrap().observed_from.clone(),
        observed_until: source.observation.snapshot().unwrap().observed_until.clone(),
        context: Some(context),
        observations,
    });
    snapshot
}

fn comparison(snapshot: Snapshot) -> culprit_core::CompilationComparisonReport {
    culprit_checks::analyze(snapshot).query_compilation.unwrap().comparison.unwrap()
}

#[test]
fn history_uses_one_original_default_read_without_summing_runtime_population_copies() {
    let directory = tempfile::tempdir().unwrap();
    let previous = snapshot(0);
    save(directory.path(), &culprit_checks::analyze(previous.clone())).unwrap();
    let mut selected = previous.clone();
    selected.id = "20200101T120030.000Z".into();
    selected.taken_at = "2020-01-01T12:00:30Z".into();
    selected.options.as_mut().unwrap().workload_population = Some(NativeIdentity::new("native-runtime-group"));
    save(directory.path(), &culprit_checks::analyze(selected.clone())).unwrap();
    let mut current = snapshot(1);
    apply(directory.path(), &mut current, Some(&selected));
    let before = current.compilations_before.as_ref().unwrap();
    assert_eq!(before.reference.as_ref().unwrap().id.as_deref(), Some(previous.id.as_str()));
    assert_eq!(before.points.len(), 1);
    let references = current.baseline_references();
    let references =
        references.iter().filter(|reference| reference.purpose == BaselinePurpose::Compilation).collect::<Vec<_>>();
    assert_eq!(references.len(), 1);
    assert!(references[0].measurement.is_none());
    let report = comparison(current.clone());
    assert_eq!(report.entries[0].compilations, "1");
    current.options.as_mut().unwrap().workload_population = selected.options.unwrap().workload_population;
    assert_eq!(
        comparison(current).entries[0].compilations,
        "1",
        "the same query-global change is not a runtime-group share"
    );
}

#[test]
fn a_compact_scan_time_baseline_and_its_provenance_survive_report_pruning() {
    let directory = tempfile::tempdir().unwrap();
    let previous = snapshot(0);
    let mut current = snapshot(1);
    apply(directory.path(), &mut current, Some(&previous));
    let expected = current.compilations_before.clone().unwrap();
    save(directory.path(), &culprit_checks::analyze(current.clone())).unwrap();
    let path = directory.path().join(format!("{}.json", current.id));
    let mut restored = load_snapshot(&path).unwrap();
    apply(directory.path(), &mut restored, None);
    assert_eq!(restored.compilations_before, Some(expected));
    let report = comparison(restored);
    assert_eq!(report.baseline.as_ref().unwrap().id.as_deref(), Some(previous.id.as_str()));
    assert_eq!(report.entries[0].compilations, "1");
    assert!(!directory.path().join(format!("{}.json", previous.id)).exists());
}

#[test]
fn original_namespace_changes_stay_explicit_and_do_not_fall_back_to_endpoint_or_older_counts() {
    let directory = tempfile::tempdir().unwrap();
    let mut previous = snapshot(0);
    let mut context = previous.operations.query_compilation.as_ref().unwrap().context.clone().unwrap();
    let culprit_core::telemetry::CollectionTarget::Database(database) = &mut context.target else { panic!() };
    database.incarnation = Some("another-native-database".into());
    previous.identity = Some(database.clone());
    previous.operations.query_compilation.as_mut().unwrap().context = Some(context);
    let mut current = snapshot(1);
    apply(directory.path(), &mut current, Some(&previous));
    let report = comparison(current);
    assert!(report.entries.is_empty());
    assert_eq!(report.issues, [CompilationComparisonIssue::DifferentSource]);
    assert!(report.baseline.is_some(), "the original unavailable comparison remains inspectable");
}

#[test]
fn a_foreign_source_attached_to_another_snapshot_is_not_a_baseline_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let mut previous = snapshot(0);
    previous.identity.as_mut().unwrap().incarnation = Some("a-different-snapshot-database".into());
    let mut current = snapshot(1);
    apply(directory.path(), &mut current, Some(&previous));
    assert!(current.compilations_before.is_none());
}

#[test]
fn scan_locator_order_preserves_subsecond_precision_without_supplying_native_counter_time() {
    let directory = tempfile::tempdir().unwrap();
    let mut before = snapshot(0);
    let mut after = snapshot(1);
    before.taken_at = "2026-11-13T10:00:00.000000001Z".into();
    after.taken_at = "2026-11-13T10:00:00.000000002Z".into();
    apply(directory.path(), &mut after, Some(&before));
    let report = comparison(after);
    assert_eq!(report.entries[0].compilations, "1");
    assert_eq!(report.observed_from.as_deref(), Some("2020-01-01T12:00:00Z"));
    assert_eq!(report.observed_until.as_deref(), Some("2020-01-01T12:01:01Z"));
    assert_eq!(report.baseline.unwrap().taken_at, before.taken_at);
}
