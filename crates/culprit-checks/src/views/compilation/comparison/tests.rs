use super::*;
use crate::views::compilation::tests::{native, observation};
use culprit_core::{DatabaseIdentity, Engine, telemetry::*};

fn compare(
    context: Option<&NativeReadContext>,
    observations: &[&OperationalObservation],
    source: Option<&SourceOutcome>,
    previous: &CompilationBaseline,
) -> Result<CompilationComparisonReport> {
    super::compare(CompilationEvidence { context, observations, source, definitions: None }, previous)
}

fn context(start: &str, end: &str) -> NativeReadContext {
    let mut database = DatabaseIdentity::server(Engine::Mssql, "db", "private-host.invalid", 1433, None);
    database.incarnation = Some("native-logical-database".into());
    NativeReadContext {
        target: database.into(),
        partition: SourcePartition::Target,
        observed_from: start.into(),
        observed_until: end.into(),
        database_writable: Some(true),
    }
}
fn before() -> NativeReadContext {
    context("2020-01-01T12:00:00Z", "2020-01-01T12:00:01Z")
}
fn after() -> NativeReadContext {
    context("2020-01-01T12:01:00Z", "2020-01-01T12:01:01Z")
}
fn source(state: SourceState) -> SourceOutcome {
    SourceOutcome { source: "query compilation fixture".into(), state, reason: None, reason_message: None }
}
fn baseline(row: &OperationalObservation) -> CompilationBaseline {
    CompilationBaseline::from_observations(&row.source, Some(before()), Some(SourceState::Collected), &[row], None)
        .unwrap()
}
fn current(count: &str) -> OperationalObservation {
    let mut row = observation(count);
    native(&mut row).last_compile_at = "2020-01-01T12:00:30Z".into();
    row
}
fn compared(row: &OperationalObservation, previous: &CompilationBaseline) -> CompilationComparisonReport {
    compare(Some(&after()), &[row], Some(&source(SourceState::Collected)), previous).unwrap()
}

#[test]
fn exact_counter_changes_above_double_precision_are_not_lifetime_count_or_memory_totals() {
    let previous = baseline(&observation("9007199254740993"));
    let row = current("9007199254740994");
    let report = compared(&row, &previous);
    assert!(report.issues.is_empty());
    assert_eq!(report.entries[0].compilations, "1");
    assert_eq!(report.entries[0].observation_key, row.key);
    assert_eq!(report.entries[0].baseline_observation_key, previous.points[0].observation_key);
    assert_eq!(report.observed_from.as_deref(), Some("2020-01-01T12:00:00Z"));
    assert_eq!(report.observed_until.as_deref(), Some("2020-01-01T12:01:01Z"));
    let json = serde_json::to_string(&report).unwrap();
    for forbidden in ["total_memory", "duration_delta", "db_time_share", "snapshot", "workload"] {
        assert!(!json.contains(forbidden), "{forbidden}");
    }
    assert_eq!(compared(&observation("9007199254740993"), &previous).entries[0].compilations, "0");
}

#[test]
fn a_native_statement_directive_is_independent_of_the_lifetime_average_cost() {
    let mut row = current("12");
    native(&mut row).statement = DefinitionPayload::sql("SELECT id FROM dbo.orders OPTION (RECOMPILE)", Engine::Mssql);
    native(&mut row).origin = StatementOrigin::Application;
    native(&mut row).average_duration_us = 1.0;
    let report = compared(&row, &baseline(&observation("2")));
    assert!(report.entries[0].explanation.contains(&t!("view.compilation.comparison.entry.policy")));
    assert!(!report.entries[0].explanation.contains("expensive"));
    native(&mut row).statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    let report = compared(&row, &baseline(&observation("2")));
    assert!(!report.entries[0].explanation.contains(&t!("view.compilation.comparison.entry.policy")));
}

#[test]
fn same_display_id_or_hash_never_substitutes_for_another_native_entry() {
    let previous = baseline(&observation("2"));
    for (key, value) in [
        ("context_settings_id", "6"),
        ("object_id", "2000"),
        ("query_hash", "FEDCBA9876543210"),
        ("initial_compile_at", "2020-01-01T06:00:01Z"),
    ] {
        let mut row = current("20");
        native(&mut row).identity.qualifiers.insert(key.into(), value.into());
        row.key = native(&mut row).key().unwrap();
        let report = compared(&row, &previous);
        assert!(report.entries.is_empty(), "{key}");
        assert_eq!((report.missing_before, report.missing_after), (1, 1));
        assert!(report.issues.contains(&Issue::UnmatchedEntries));
    }
}

#[test]
fn missing_native_namespace_or_changed_database_cannot_be_repaired_by_endpoints_or_role() {
    let previous = baseline(&observation("2"));
    let row = current("3");
    for context in [
        None,
        Some({
            let mut c = after();
            if let CollectionTarget::Database(db) = &mut c.target {
                db.incarnation = None;
            }
            c
        }),
        Some({
            let mut c = after();
            if let CollectionTarget::Database(db) = &mut c.target {
                db.incarnation = Some("different".into());
            }
            c
        }),
    ] {
        let report = compare(context.as_ref(), &[&row], Some(&source(SourceState::Collected)), &previous).unwrap();
        assert!(report.entries.is_empty() && report.observed_from.is_none());
        assert_eq!(report.issues.len(), 1);
    }
    let mut alias = after();
    if let CollectionTarget::Database(db) = &mut alias.target {
        db.location = DatabaseIdentity::server(Engine::Mssql, "db", "another-native-alias", 1433, None).location;
    }
    alias.database_writable = Some(false);
    let report = compare(Some(&alias), &[&row], Some(&source(SourceState::Collected)), &previous).unwrap();
    assert_eq!(report.entries[0].compilations, "1", "query-global counts follow native namespace across reader roles");
}

#[test]
fn partial_reads_keep_present_deltas_and_never_turn_missing_rows_into_zero() {
    let previous = baseline(&observation("2"));
    let row = current("3");
    let report = compare(Some(&after()), &[&row], Some(&source(SourceState::Partial)), &previous).unwrap();
    assert_eq!(report.entries[0].compilations, "1");
    assert!(report.issues.contains(&Issue::PartialRead));
    let report = compare(Some(&after()), &[], Some(&source(SourceState::Partial)), &previous).unwrap();
    assert!(report.entries.is_empty());
    assert_eq!(report.missing_after, 1);
    assert!(report.issues.contains(&Issue::UnmatchedEntries));
    let report = compare(Some(&after()), &[], Some(&source(SourceState::Unavailable)), &previous).unwrap();
    assert_eq!(report.issues, [Issue::SourceUnavailable]);
}

#[test]
fn reordered_clocks_counter_decreases_and_changed_reset_markers_withhold_comparisons() {
    let previous = baseline(&observation("20"));
    let row = current("3");
    assert!(compared(&row, &previous).issues.contains(&Issue::CounterChanged));
    let row = current("21");
    for context in [before(), context("2020-01-01T11:59:00Z", "2020-01-01T12:00:00Z")] {
        let report = compare(Some(&context), &[&row], Some(&source(SourceState::Collected)), &previous).unwrap();
        assert_eq!(report.issues, [Issue::InvalidObservationOrder]);
        assert!(report.entries.is_empty());
    }
    let mut changed = previous.clone();
    changed.points[0].reset = culprit_core::ResetMarker::Generation("another-counter-epoch".into());
    assert!(compared(&row, &changed).issues.contains(&Issue::CounterChanged));
    changed.points[0].reset = culprit_core::ResetMarker::Unknown;
    changed.points[0].last_compile_at = "2021-01-01T00:00:00Z".into();
    assert!(compared(&row, &changed).issues.contains(&Issue::NativeClockConflict));
    changed.points[0].last_compile_at = "not-a-clock".into();
    assert!(compare(Some(&after()), &[&row], Some(&source(SourceState::Collected)), &changed).is_err());
}

#[test]
fn a_native_compile_clock_outside_its_original_read_or_without_a_counter_change_is_not_an_interval() {
    let mut previous = baseline(&observation("2"));
    previous.points[0].last_compile_at = "2020-01-01T12:00:02Z".into();
    assert!(compared(&current("3"), &previous).issues.contains(&Issue::NativeClockConflict));
    previous.points[0].last_compile_at = "2020-01-01T05:59:59Z".into();
    assert!(compared(&current("3"), &previous).issues.contains(&Issue::NativeClockConflict));
    let previous = baseline(&observation("2"));
    let report = compared(&current("2"), &previous);
    assert!(report.entries.is_empty());
    assert_eq!(report.issues, [Issue::NativeClockConflict]);
}

#[test]
fn a_retained_baseline_reference_cannot_name_a_different_native_database() {
    let mut previous = baseline(&observation("2"));
    let mut database = previous.context.as_ref().unwrap().target.database().unwrap().clone();
    database.incarnation = Some("a-foreign-native-database".into());
    previous.reference = Some(culprit_core::ScanReference {
        id: Some("a-real-before-report".into()),
        taken_at: "2020-01-01T12:00:01Z".into(),
        database: Some(database),
    });
    assert!(!previous.is_valid());
    assert!(compare(Some(&after()), &[&current("3")], Some(&source(SourceState::Collected)), &previous).is_err());
}

#[test]
fn duplicate_receipts_corrupt_counts_and_foreign_sources_are_rejected() {
    let mut previous = baseline(&observation("2"));
    let row = current("3");
    assert!(compare(Some(&after()), &[&row, &row], Some(&source(SourceState::Collected)), &previous).is_err());
    previous.points.push(previous.points[0].clone());
    assert!(compare(Some(&after()), &[&row], Some(&source(SourceState::Collected)), &previous).is_err());
    previous.points.pop();
    previous.points[0].compilations = "not-an-integer".into();
    assert!(compare(Some(&after()), &[&row], Some(&source(SourceState::Collected)), &previous).is_err());
    previous = baseline(&observation("2"));
    previous.source = "another-native-source".into();
    assert_eq!(compared(&row, &previous).issues, [Issue::DifferentSource]);
}

#[test]
fn native_nanosecond_order_is_preserved_without_float_epoch_rounding() {
    let previous = baseline(&observation("2"));
    let context = context("2020-01-01T12:00:01.000000001Z", "2020-01-01T12:00:01.000000002Z");
    let mut row = current("3");
    native(&mut row).last_compile_at = "2020-01-01T12:00:01.000000001Z".into();
    let report = compare(Some(&context), &[&row], Some(&source(SourceState::Collected)), &previous).unwrap();
    assert_eq!(report.entries[0].compilations, "1");
    assert_eq!(report.observed_until.as_deref(), Some("2020-01-01T12:00:01.000000002Z"));
}

#[test]
fn compact_baselines_have_independent_count_and_serialized_byte_limits_without_sql_or_memory_copies() {
    let row = observation("2");
    let rows = vec![&row; MAX_QUERY_COMPILATION_OBSERVATIONS + 1];
    assert!(
        CompilationBaseline::from_observations(&row.source, Some(before()), Some(SourceState::Partial), &rows, None)
            .is_err()
    );
    let rows: Vec<_> = (0..1100)
        .map(|index| {
            let mut row = observation("2");
            native(&mut row).identity.query_id = (index + 1).to_string();
            for property in 0..16 {
                native(&mut row).identity.qualifiers.insert(format!("native-context-{property}"), "r".repeat(512));
            }
            row.key = native(&mut row).key().unwrap();
            row
        })
        .collect();
    let refs: Vec<_> = rows.iter().collect();
    let error =
        CompilationBaseline::from_observations(&row.source, Some(before()), Some(SourceState::Partial), &refs, None)
            .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::ByteLimit)));
    let json = serde_json::to_value(baseline(&row)).unwrap();
    for field in ["statement", "memory", "average_duration_us"] {
        assert!(json["points"][0].get(field).is_none());
    }
}

#[test]
fn sharing_preserves_exact_counts_and_native_equality_without_exposing_read_namespace_or_baseline_location() {
    let mut snapshot: culprit_core::Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    let row = current("3");
    snapshot.operations.query_compilation = Some(CompilationSnapshot {
        source: row.source.clone(),
        context: Some(after()),
        observed_from: "2020-01-01T12:01:00Z".into(),
        observed_until: "2020-01-01T12:01:01Z".into(),
        observations: vec![row],
    });
    let mut previous = baseline(&observation("2"));
    previous.reference = Some(culprit_core::ScanReference {
        id: Some("private-prior-report".into()),
        taken_at: "2020-01-01T12:00:01Z".into(),
        database: previous.context.as_ref().and_then(|context| context.target.database().cloned()),
    });
    snapshot.compilations_before = Some(previous);
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    let json = serde_json::to_string(&shared).unwrap();
    for secret in ["private-host.invalid", "native-logical-database", "private-prior-report"] {
        assert!(!json.contains(secret), "{secret}");
    }
    let current = shared.operations.query_compilation.as_ref().unwrap();
    let previous = shared.compilations_before.as_ref().unwrap();
    assert!(current.is_valid() && previous.is_valid());
    let rows: Vec<_> = current.observations.iter().collect();
    let report = compare(current.context.as_ref(), &rows, Some(&source(SourceState::Collected)), previous).unwrap();
    assert_eq!(report.entries[0].compilations, "1");
    assert_eq!(
        serde_json::to_value(&shared).unwrap(),
        serde_json::to_value(culprit_core::redact::redact_snapshot(&shared)).unwrap()
    );
}
