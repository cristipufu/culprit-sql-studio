use super::*;
mod pvs_investigation;
use culprit_core::{
    CleanupHolder, CleanupMechanism, MeasurementScope, TransactionActivity, telemetry::PayloadUnavailable,
};

fn source(name: &str, state: SourceState) -> SourceOutcome {
    SourceOutcome { source: name.into(), state, reason: None, reason_message: None }
}

fn persistent() -> OperationalObservation {
    let store = serde_json::from_value::<MssqlPersistentVersionStore>(serde_json::json!({
        "available_columns": ["persistent_version_store_size_kb", "current_aborted_transaction_count",
            "oldest_active_transaction_id", "oldest_aborted_transaction_id", "min_transaction_timestamp",
            "pvs_off_row_page_skipped_low_water_mark", "pvs_off_row_page_skipped_transaction_not_cleaned",
            "pvs_off_row_page_skipped_oldest_active_xdesid", "pvs_off_row_page_skipped_min_useful_xts",
            "pvs_off_row_page_skipped_oldest_snapshot", "pvs_off_row_page_skipped_oldest_aborted_xdesid",
            "offrow_version_cleaner_start_time", "offrow_version_cleaner_end_time"],
        "off_row_kibibytes": "9007199254740993", "aborted_transactions": "7",
        "oldest_active_transaction_id": "42", "oldest_aborted_transaction_id": "-42",
        "minimum_transaction_timestamp": "-9223372036854775808",
        "off_row_cleanup": {"started_without_offset": "2020-01-01T11:00:00.003", "ended_without_offset": "2020-01-01T10:00:00.003"},
        "aborted_cleanup": {},
        "pages_skipped_for_secondaries": "0", "pages_skipped_for_aborted_transactions": "7",
        "pages_skipped_for_oldest_active_transaction": "0", "pages_skipped_for_long_snapshot_scan": "0",
        "pages_skipped_for_online_index_rebuild": "0", "pages_skipped_for_oldest_aborted_transaction": "0"
    })).unwrap();
    assert!(store.is_valid());
    OperationalObservation {
        source: "native pvs".into(),
        key: "persistent-version-store".into(),
        fact: OperationalFact::MssqlPersistentVersionStore(Box::new(store)),
    }
}

fn pvs(row: &mut OperationalObservation) -> &mut MssqlPersistentVersionStore {
    let OperationalFact::MssqlPersistentVersionStore(store) = &mut row.fact else { panic!() };
    store
}

fn native_report(rows: &[&OperationalObservation], states: &[SourceOutcome]) -> CleanupReport {
    explain(CleanupEvidence { retention: None, native: rows }, states).unwrap()
}

#[test]
fn positive_native_skip_reasons_stay_separate_and_online_rebuild_is_not_inferred_from_snapshot_spelling() {
    let mut row = persistent();
    let store = pvs(&mut row);
    store.pages_skipped_for_secondaries = Some("1".into());
    store.pages_skipped_for_oldest_active_transaction = Some("9007199254740993".into());
    store.pages_skipped_for_long_snapshot_scan = Some("2".into());
    store.pages_skipped_for_online_index_rebuild = Some("3".into());
    store.pages_skipped_for_oldest_aborted_transaction = Some("4".into());
    let report = native_report(&[&row], &[source(&row.source, SourceState::Collected)]);
    let [entry] = report.entries.as_slice() else { panic!("one original row") };
    for mechanism in [
        CleanupMechanism::PersistentVersionStore,
        CleanupMechanism::SecondaryLowWatermark,
        CleanupMechanism::OldestActiveTransaction,
        CleanupMechanism::LongSnapshotScan,
        CleanupMechanism::OnlineIndexRebuild,
        CleanupMechanism::OldestAbortedTransaction,
    ] {
        assert!(entry.mechanisms.contains(&mechanism));
    }
    assert_eq!(entry.reference, native_reference(&row));
    assert!(entry.explanation.contains(&t!("view.cleanup.pvs.online_rebuild", pages = "3")));
    assert!(entry.metrics.iter().any(|metric| metric.text.as_deref() == Some("9007199254740993")));
    assert!(entry.metrics.iter().any(|metric| metric.text.as_deref() == Some("-9223372036854775808")));
    assert!(entry.metrics.iter().any(|metric| metric.text.as_deref() == Some("2020-01-01T10:00:00.003")));
    assert!(report.boundaries.is_empty());
    let json = serde_json::to_value(&report).unwrap();
    assert!(json.get("severity").is_none() && json.get("total_version_bytes").is_none());
    assert!(json["entries"][0].get("owner").is_none() && json["entries"][0].get("fix_sql").is_none());
}

#[test]
fn transaction_not_cleaned_and_raw_size_do_not_identify_aborted_transaction_retention() {
    let row = persistent();
    let report = native_report(&[&row], &[source(&row.source, SourceState::Collected)]);
    assert_eq!(report.entries[0].mechanisms, [CleanupMechanism::PersistentVersionStore]);
    assert!(report.entries[0].explanation.contains(&t!("view.cleanup.pvs.threshold")));
    assert!(report.entries[0].explanation.contains(&t!("view.cleanup.pvs.no_recorded_cause")));
    assert!(
        report.entries[0].metrics.iter().any(|metric| metric.key == "pvs_off_row_page_skipped_transaction_not_cleaned"
            && metric.text.as_deref() == Some("7"))
    );
}

#[test]
fn native_zero_and_unavailable_optional_fields_remain_distinct_without_invented_rates() {
    let mut row = persistent();
    let zero = native_report(&[&row], &[source(&row.source, SourceState::Collected)]);
    assert!(
        zero.entries[0].metrics.iter().any(|metric| metric.key == "pvs_off_row_page_skipped_oldest_active_xdesid"
            && metric.text.as_deref() == Some("0"))
    );
    pvs(&mut row).pages_skipped_for_oldest_active_transaction = None;
    let unknown = native_report(&[&row], &[source(&row.source, SourceState::Collected)]);
    assert!(
        !unknown.entries[0].metrics.iter().any(|metric| metric.key == "pvs_off_row_page_skipped_oldest_active_xdesid")
    );
    assert_eq!(unknown.entries[0].mechanisms, zero.entries[0].mechanisms);
    assert!(unknown.limitations.contains(&t!("view.cleanup.limit.native_clocks")));
}

fn transaction(confirmed: bool) -> MssqlCleanupTransaction {
    MssqlCleanupTransaction {
        session: NativeActivityIdentity { id: "62".into(), generation: "native-session-generation".into() },
        transaction: NativeActivityIdentity { id: "42".into(), generation: "2020-01-01T10:00:00.003".into() },
        observed_time_without_offset: "2020-01-01T11:00:00.003".into(),
        age_seconds: Some("3600".into()),
        program: None,
        native_status: None,
        log_record_count: confirmed.then(|| "1".into()),
        log_bytes_used: None,
        begin_lsn: Some("1234567890123456789012345".into()),
        row_version_participant: true,
        snapshot_isolation: Some(confirmed),
        transaction_sequence_number: Some("7".into()),
        versioning_age_seconds: Some("3500".into()),
    }
}

fn horizon() -> OperationalObservation {
    OperationalObservation {
        source: "native transactions".into(),
        key: "oldest-cleanup-transaction".into(),
        fact: OperationalFact::MssqlCleanupHorizon(MssqlCleanupHorizon {
            oldest_confirmed: None,
            oldest_unattributed_versioning: None,
        }),
    }
}

#[test]
fn empty_qualifying_horizon_accepts_both_native_semantic_and_warehouse_row_coverage() {
    let mut row = horizon();
    for state in [SourceState::Empty, SourceState::Collected, SourceState::Partial] {
        let report = native_report(&[&row], &[source(&row.source, state)]);
        assert_eq!(report.sources[0].state, state);
        assert_eq!(report.entries.len(), 1);
        assert!(report.entries[0].mechanisms.is_empty());
        assert_eq!(report.entries[0].explanation, t!("view.cleanup.transaction.empty"));
    }
    let OperationalFact::MssqlCleanupHorizon(native) = &mut row.fact else { panic!() };
    native.oldest_unattributed_versioning = Some(Box::new(transaction(false)));
    assert!(row.is_valid());
    assert!(!native_source_is_consistent(Some(&source(&row.source, SourceState::Empty)), &[&row]));
    assert!(!native_source_is_consistent(Some(&source(&row.source, SourceState::Unavailable)), &[&row]));
    let report = native_report(&[&row], &[source(&row.source, SourceState::Partial)]);
    assert_eq!(report.entries[0].mechanisms, [CleanupMechanism::UnattributedVersioning]);
}

#[test]
fn matching_transaction_numbers_in_independent_native_reads_never_authorize_a_query_owner_join() {
    let mut row = horizon();
    let OperationalFact::MssqlCleanupHorizon(native) = &mut row.fact else { panic!() };
    native.oldest_confirmed = Some(Box::new(transaction(true)));
    native.oldest_unattributed_versioning = Some(Box::new(transaction(false)));
    let store = persistent();
    let report = native_report(
        &[&row, &store],
        &[source(&row.source, SourceState::Partial), source(&store.source, SourceState::Collected)],
    );
    assert_eq!(report.entries.len(), 2);
    let horizon = report.entries.iter().find(|entry| entry.reference.source == row.source).unwrap();
    assert_eq!(
        horizon.mechanisms,
        [CleanupMechanism::ConfirmedDatabaseTransaction, CleanupMechanism::UnattributedVersioning]
    );
    assert!(horizon.metrics.iter().any(|metric| metric.text.as_deref() == Some("1234567890123456789012345")));
    assert!(report.limitations.contains(&t!("view.cleanup.limit.independent_reads")));
    assert!(horizon.reference.identity.is_none());
}

fn holder(name: &str, data: u64, catalog: u64) -> RetentionObservation {
    RetentionObservation {
        source: "native retainers".into(),
        identity: NativeIdentity::new(name),
        observed_at: "2026-11-13T10:00:00Z".into(),
        scope: MeasurementScope::Server,
        holder: CleanupHolder::ReplicationSlot,
        name: name.into(),
        horizons: vec![
            RetentionHorizon {
                protects: vec![CleanupHorizon::Data],
                native_name: "xmin".into(),
                native_value: "1234".into(),
                transactions_behind: Some(data.to_string()),
            },
            RetentionHorizon {
                protects: vec![CleanupHorizon::Catalog],
                native_name: "catalog_xmin".into(),
                native_value: "1000".into(),
                transactions_behind: Some(catalog.to_string()),
            },
        ],
        session: None,
        unresolved_session_id: Some("62".into()),
        transaction: None,
        query: None,
        client: None,
        activity: TransactionActivity::SlotInactive,
        native_state: None,
        age_seconds: None,
        database: None,
        code_ownership: PayloadUnavailable::NotCollected,
    }
}

fn population() -> RetentionSnapshot {
    RetentionSnapshot {
        holders: vec![holder("a", 200, 250), holder("b", 200, 100)],
        sources: vec![RetentionSourceCoverage { source: "native retainers".into(), state: SourceState::Collected }],
        coverage: RetentionCoverage {
            native_sources: vec!["native retainers".into()],
            qualified_horizons: vec![CleanupHorizon::Data, CleanupHorizon::Catalog],
        },
    }
}

#[test]
fn independently_qualified_retention_boundaries_preserve_all_ties_and_unknown_ordering() {
    let mut population = population();
    let report = explain(CleanupEvidence { retention: Some(&population), native: &[] }, &[]).unwrap();
    let data = report.boundaries.iter().find(|boundary| boundary.horizon == CleanupHorizon::Data).unwrap();
    assert!(data.complete);
    assert_eq!(data.oldest_observed.len(), 2);
    assert!(data.oldest_observed.iter().all(|reference| reference.identity.is_some()));
    let catalog = report.boundaries.iter().find(|boundary| boundary.horizon == CleanupHorizon::Catalog).unwrap();
    assert!(catalog.complete);
    assert_eq!(catalog.oldest_observed.len(), 1);
    assert_eq!(catalog.oldest_observed[0].identity.as_ref().unwrap().id, "a");
    assert!(
        !report.boundaries.iter().find(|boundary| boundary.horizon == CleanupHorizon::SharedCatalog).unwrap().complete
    );
    population.holders[1].horizons[0].transactions_behind = None;
    let report = explain(CleanupEvidence { retention: Some(&population), native: &[] }, &[]).unwrap();
    assert!(!report.boundaries.iter().find(|boundary| boundary.horizon == CleanupHorizon::Data).unwrap().complete);
    assert!(report.limitations.contains(&t!("view.cleanup.limit.order_unknown")));
    assert!(report.entries.iter().all(|entry| entry.mechanisms == [CleanupMechanism::ReplicationSlotHorizon]));
    assert!(report.entries[0].explanation.contains(&t!("view.cleanup.retainer.unresolved_session")));
}

#[test]
fn missing_or_partial_retention_metadata_preserves_positive_holders_without_complete_boundaries() {
    let population = population();
    let mut rows = population
        .holders
        .iter()
        .map(|holder| OperationalObservation {
            source: holder.source.clone(),
            key: holder.key().unwrap(),
            fact: OperationalFact::Retention(Box::new(holder.clone())),
        })
        .collect::<Vec<_>>();
    let missing = native_report(
        &rows.iter().collect::<Vec<_>>(),
        &[source("native retainers", SourceState::Collected), source(RetentionCoverage::SOURCE, SourceState::Partial)],
    );
    assert_eq!(missing.entries.len(), 2);
    assert!(missing.boundaries.is_empty());
    assert!(missing.limitations.contains(&t!("view.cleanup.limit.coverage_unknown")));
    rows.push(OperationalObservation {
        source: RetentionCoverage::SOURCE.into(),
        key: population.coverage.key().unwrap(),
        fact: OperationalFact::RetentionCoverage(population.coverage.clone()),
    });
    for state in [SourceState::Partial, SourceState::Collected] {
        let report = native_report(
            &rows.iter().collect::<Vec<_>>(),
            &[source("native retainers", SourceState::Collected), source(RetentionCoverage::SOURCE, state)],
        );
        assert_eq!(report.entries.len(), 2);
        assert_eq!(
            report.boundaries.iter().find(|boundary| boundary.horizon == CleanupHorizon::Data).unwrap().complete,
            state == SourceState::Collected
        );
    }
    assert!(
        explain(CleanupEvidence { retention: Some(&population), native: &rows.iter().collect::<Vec<_>>() }, &[])
            .is_err(),
        "one retained population must not appear twice"
    );
}

#[test]
fn duplicates_incompatible_sources_and_unsupported_native_facts_fail_without_hiding_other_kinds() {
    let row = persistent();
    assert!(explain(CleanupEvidence { retention: None, native: &[&row, &row] }, &[]).is_err());
    assert!(
        explain(CleanupEvidence { retention: None, native: &[&row] }, &[source(&row.source, SourceState::Unavailable)])
            .is_err()
    );
    let mut wrong = row.clone();
    pvs(&mut wrong).off_row_kibibytes = Some("-1".into());
    assert!(explain(CleanupEvidence { retention: None, native: &[&wrong] }, &[]).is_err());
    let empty = horizon();
    assert!(!native_source_is_consistent(Some(&source("different source", SourceState::Empty)), &[&empty]));
    let counter = NativeCounterSample::gauge(
        "not-a-cleanup-mechanism",
        10,
        CounterUnit::Count,
        MeasurementScope::Server,
        "other",
        "counter",
    );
    let wrong = OperationalObservation {
        source: row.source,
        key: counter.key.clone(),
        fact: OperationalFact::Counter(counter),
    };
    assert!(explain(CleanupEvidence { retention: None, native: &[&wrong] }, &[]).is_err());
}

fn confirmed_transaction(
    snapshot: Option<bool>,
    log_records: Option<&str>,
    versioning: bool,
) -> OperationalObservation {
    let mut transaction = transaction(true);
    transaction.snapshot_isolation = snapshot;
    transaction.log_record_count = log_records.map(str::to_owned);
    transaction.row_version_participant = versioning;
    if !versioning {
        transaction.transaction_sequence_number = None;
        transaction.versioning_age_seconds = None;
    }
    let mut row = horizon();
    let OperationalFact::MssqlCleanupHorizon(native) = &mut row.fact else { panic!() };
    native.oldest_confirmed = Some(Box::new(transaction));
    assert!(row.is_valid());
    row
}

#[test]
fn the_same_native_transaction_distinguishes_snapshot_lifetime_from_logged_work() {
    for (snapshot, logs, versioning, detail, investigation) in [
        (
            Some(true),
            Some("0"),
            true,
            t!("view.cleanup.transaction.context.snapshot_zero_log"),
            t!("view.cleanup.transaction.investigate_snapshot"),
        ),
        (
            Some(true),
            Some("3"),
            true,
            t!("view.cleanup.transaction.context.snapshot_logged"),
            t!("view.cleanup.transaction.investigate_snapshot_logged"),
        ),
        (
            Some(true),
            None,
            true,
            t!("view.cleanup.transaction.context.snapshot_unknown_log"),
            t!("view.cleanup.transaction.investigate_snapshot"),
        ),
        (
            Some(false),
            Some("3"),
            true,
            t!("view.cleanup.transaction.context.logged_versioning"),
            t!("view.cleanup.transaction.investigate_logged"),
        ),
        (
            None,
            Some("3"),
            true,
            t!("view.cleanup.transaction.context.logged_versioning"),
            t!("view.cleanup.transaction.investigate_logged"),
        ),
        (
            None,
            Some("3"),
            false,
            t!("view.cleanup.transaction.context.logged"),
            t!("view.cleanup.transaction.investigate_logged"),
        ),
    ] {
        let row = confirmed_transaction(snapshot, logs, versioning);
        let original = serde_json::to_value(&row).unwrap();
        let report = native_report(&[&row], &[source(&row.source, SourceState::Collected)]);
        let [entry] = report.entries.as_slice() else { panic!("one original horizon") };
        assert_eq!(entry.mechanisms, [CleanupMechanism::ConfirmedDatabaseTransaction]);
        assert_eq!(entry.reference, native_reference(&row));
        assert_eq!(entry.explanation, format!("{} {detail}", t!("view.cleanup.transaction.confirmed")));
        assert_eq!(entry.investigation, format!("{} {investigation}", t!("view.cleanup.transaction.investigate")));
        assert!(report.boundaries.is_empty());
        assert_eq!(serde_json::to_value(&row).unwrap(), original);
    }
}

#[test]
fn zero_logged_work_and_unavailable_logged_work_remain_different_native_facts() {
    let zero = confirmed_transaction(Some(true), Some("0"), true);
    let unknown = confirmed_transaction(Some(true), None, true);
    let zero = native_report(&[&zero], &[source(&zero.source, SourceState::Collected)]);
    let unknown = native_report(&[&unknown], &[source(&unknown.source, SourceState::Collected)]);
    assert!(zero.entries[0].explanation.contains(&t!("view.cleanup.transaction.context.snapshot_zero_log")));
    assert!(unknown.entries[0].explanation.contains(&t!("view.cleanup.transaction.context.snapshot_unknown_log")));
    assert!(
        zero.entries[0]
            .metrics
            .iter()
            .any(|metric| { metric.key == "confirmed:log_record_count" && metric.text.as_deref() == Some("0") })
    );
    assert!(!unknown.entries[0].metrics.iter().any(|metric| metric.key == "confirmed:log_record_count"));
    assert_eq!(zero.entries[0].reference, unknown.entries[0].reference);
}

#[test]
fn unattributed_versioning_cannot_borrow_database_qualification_from_a_matching_pvs_number() {
    let mut row = horizon();
    let OperationalFact::MssqlCleanupHorizon(native) = &mut row.fact else { panic!() };
    native.oldest_unattributed_versioning = Some(Box::new(transaction(false)));
    let mut store = persistent();
    pvs(&mut store).pages_skipped_for_oldest_active_transaction = Some("9007199254740993".into());
    let report = native_report(
        &[&row, &store],
        &[source(&row.source, SourceState::Partial), source(&store.source, SourceState::Collected)],
    );
    let entry = report.entries.iter().find(|entry| entry.reference == native_reference(&row)).unwrap();
    assert_eq!(entry.mechanisms, [CleanupMechanism::UnattributedVersioning]);
    assert_eq!(entry.explanation, t!("view.cleanup.transaction.unattributed"));
    assert_eq!(entry.investigation, t!("view.cleanup.transaction.investigate"));
    assert!(report.limitations.contains(&t!("view.cleanup.limit.independent_reads")));
}

#[test]
fn independent_pvs_causes_and_capture_order_cannot_change_a_transactions_recorded_mechanism() {
    let row = confirmed_transaction(None, Some("3"), false);
    let baseline = native_report(&[&row], &[source(&row.source, SourceState::Collected)]);
    let mut store = persistent();
    pvs(&mut store).pages_skipped_for_oldest_active_transaction = Some("9007199254740993".into());
    for rows in [[&row, &store], [&store, &row]] {
        let report = native_report(
            &rows,
            &[source(&row.source, SourceState::Collected), source(&store.source, SourceState::Collected)],
        );
        let entry = report.entries.iter().find(|entry| entry.reference == native_reference(&row)).unwrap();
        assert_eq!(serde_json::to_value(entry).unwrap(), serde_json::to_value(&baseline.entries[0]).unwrap());
    }
    let ended = horizon();
    let report = native_report(
        &[&ended, &store],
        &[source(&ended.source, SourceState::Empty), source(&store.source, SourceState::Collected)],
    );
    let entry = report.entries.iter().find(|entry| entry.reference == native_reference(&ended)).unwrap();
    assert!(entry.mechanisms.is_empty());
    assert_eq!(entry.explanation, t!("view.cleanup.transaction.empty"));
}

#[test]
fn positive_native_transaction_context_survives_partial_or_unknown_coverage_without_completeness() {
    let mut row = confirmed_transaction(Some(true), Some("3"), true);
    let OperationalFact::MssqlCleanupHorizon(native) = &mut row.fact else { panic!() };
    native.oldest_confirmed.as_mut().unwrap().program = Some("private-program-and-query-canary".into());
    for sources in [vec![source(&row.source, SourceState::Partial)], vec![]] {
        let report = native_report(&[&row], &sources);
        assert!(report.entries[0].explanation.contains(&t!("view.cleanup.transaction.context.snapshot_logged")));
        assert_eq!(report.entries[0].reference, native_reference(&row));
        assert!(report.boundaries.is_empty());
        assert!(report.limitations.contains(&if sources.is_empty() {
            t!("view.cleanup.limit.source_unknown")
        } else {
            t!("view.cleanup.limit.source_partial")
        }));
        let encoded = serde_json::to_value(&report).unwrap();
        assert!(encoded.get("severity").is_none());
        assert!(encoded["entries"][0].get("owner").is_none());
        assert!(encoded["entries"][0].get("fix_sql").is_none());
        assert!(!encoded.to_string().contains("private-program-and-query-canary"));
    }
}

#[test]
fn unavailable_sources_and_invalid_generations_cannot_supply_a_transaction_mechanism() {
    let mut row = confirmed_transaction(Some(true), Some("0"), true);
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::NotApplicable] {
        assert!(explain(CleanupEvidence { retention: None, native: &[&row] }, &[source(&row.source, state)]).is_err());
    }
    for logs in [None, Some("0")] {
        let mut unqualified = row.clone();
        let OperationalFact::MssqlCleanupHorizon(native) = &mut unqualified.fact else { panic!() };
        let transaction = native.oldest_confirmed.as_mut().unwrap();
        transaction.snapshot_isolation = Some(false);
        transaction.log_record_count = logs.map(str::to_owned);
        assert!(!unqualified.is_valid(), "versioning membership alone cannot qualify this database");
        assert!(
            explain(
                CleanupEvidence { retention: None, native: &[&unqualified] },
                &[source(&unqualified.source, SourceState::Collected)]
            )
            .is_err()
        );
    }
    let OperationalFact::MssqlCleanupHorizon(native) = &mut row.fact else { panic!() };
    native.oldest_confirmed.as_mut().unwrap().session.generation.clear();
    assert!(!row.is_valid());
    assert!(
        explain(CleanupEvidence { retention: None, native: &[&row] }, &[source(&row.source, SourceState::Collected)])
            .is_err()
    );
}
