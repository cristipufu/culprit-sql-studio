use super::*;

type SetReason = fn(&mut MssqlPersistentVersionStore, Option<String>);

fn reasons() -> [(SetReason, CleanupMechanism, &'static str); 5] {
    [
        (
            |store, value| store.pages_skipped_for_secondaries = value,
            CleanupMechanism::SecondaryLowWatermark,
            "view.cleanup.pvs.investigate.secondary",
        ),
        (
            |store, value| store.pages_skipped_for_oldest_active_transaction = value,
            CleanupMechanism::OldestActiveTransaction,
            "view.cleanup.pvs.investigate.active",
        ),
        (
            |store, value| store.pages_skipped_for_long_snapshot_scan = value,
            CleanupMechanism::LongSnapshotScan,
            "view.cleanup.pvs.investigate.snapshot_scan",
        ),
        (
            |store, value| store.pages_skipped_for_online_index_rebuild = value,
            CleanupMechanism::OnlineIndexRebuild,
            "view.cleanup.pvs.investigate.online_rebuild",
        ),
        (
            |store, value| store.pages_skipped_for_oldest_aborted_transaction = value,
            CleanupMechanism::OldestAbortedTransaction,
            "view.cleanup.pvs.investigate.aborted",
        ),
    ]
}

#[test]
fn each_positive_native_skip_reason_selects_only_its_qualified_follow_up() {
    for (set, mechanism, message) in reasons() {
        let mut row = persistent();
        set(pvs(&mut row), Some("9007199254740993".into()));
        let report = native_report(&[&row], &[source(&row.source, SourceState::Partial)]);
        let [entry] = report.entries.as_slice() else { panic!("one original native PVS row") };
        assert_eq!(entry.mechanisms, [CleanupMechanism::PersistentVersionStore, mechanism]);
        assert_eq!(entry.investigation, culprit_core::i18n::t(message, &[]));
        assert_eq!(entry.reference, native_reference(&row));
        assert!(entry.explanation.contains("9007199254740993"));
        assert_eq!(entry.verify, t!("view.cleanup.pvs.verify"));
        assert!(report.limitations.contains(&t!("view.cleanup.limit.source_partial")));
        for (_, _, other) in reasons().into_iter().filter(|(_, _, key)| *key != message) {
            assert!(!entry.investigation.contains(&culprit_core::i18n::t(other, &[])));
        }
    }
}

#[test]
fn zero_unknown_size_and_minimum_threshold_context_keep_the_original_generic_advice() {
    for value in [Some("0".into()), None] {
        let mut row = persistent();
        for (set, _, _) in reasons() {
            set(pvs(&mut row), value.clone());
        }
        pvs(&mut row).pages_skipped_for_aborted_transactions = Some(u64::MAX.to_string());
        let report = native_report(&[&row], &[source(&row.source, SourceState::Collected)]);
        let entry = &report.entries[0];
        assert_eq!(entry.mechanisms, [CleanupMechanism::PersistentVersionStore]);
        assert_eq!(entry.investigation, t!("view.cleanup.pvs.investigate"));
        assert!(entry.explanation.contains(&t!("view.cleanup.pvs.no_recorded_cause")));
        assert!(entry.explanation.contains(&t!("view.cleanup.pvs.threshold")));
    }
}

#[test]
fn competing_positive_reasons_keep_independent_actions_without_a_dominant_cause_or_owner() {
    let mut row = persistent();
    for (ordinal, (set, _, _)) in reasons().into_iter().enumerate() {
        set(pvs(&mut row), Some((ordinal + 1).to_string()));
    }
    let report = native_report(&[&row], &[source(&row.source, SourceState::Collected)]);
    let entry = &report.entries[0];
    let expected = reasons().into_iter().map(|(_, _, key)| culprit_core::i18n::t(key, &[])).collect::<Vec<_>>();
    assert_eq!(entry.investigation, expected.join(" "));
    assert_eq!(entry.mechanisms.len(), 6);
    let value = serde_json::to_value(&report).unwrap();
    assert!(value.get("severity").is_none());
    for unsupported in ["owner", "sql", "fix_sql", "total_version_bytes", "causal_share"] {
        assert!(value["entries"][0].get(unsupported).is_none());
    }
}

#[test]
fn another_native_row_or_matching_transaction_number_cannot_supply_a_pvs_skip_reason() {
    let mut online = persistent();
    online.source = "online-native-pvs".into();
    pvs(&mut online).pages_skipped_for_online_index_rebuild = Some("3".into());
    let unrelated = persistent();
    let mut active = horizon();
    let OperationalFact::MssqlCleanupHorizon(transaction_horizon) = &mut active.fact else { unreachable!() };
    transaction_horizon.oldest_confirmed = Some(Box::new(transaction(true)));
    assert_eq!(pvs(&mut online).oldest_active_transaction_id.as_deref(), Some("42"));
    assert_eq!(transaction_horizon.oldest_confirmed.as_ref().unwrap().transaction.id, "42");
    let report = native_report(
        &[&online, &unrelated, &active],
        &[
            source(&online.source, SourceState::Collected),
            source(&unrelated.source, SourceState::Collected),
            source(&active.source, SourceState::Collected),
        ],
    );
    let online_entry = report.entries.iter().find(|entry| entry.reference.source == online.source).unwrap();
    assert_eq!(online_entry.investigation, t!("view.cleanup.pvs.investigate.online_rebuild"));
    let other_entry = report.entries.iter().find(|entry| entry.reference.source == unrelated.source).unwrap();
    assert_eq!(other_entry.investigation, t!("view.cleanup.pvs.investigate"));
    assert!(!online_entry.mechanisms.contains(&CleanupMechanism::ConfirmedDatabaseTransaction));
    assert!(!online_entry.mechanisms.contains(&CleanupMechanism::OldestActiveTransaction));
    assert_eq!(online_entry.reference.identity, None);
}

#[test]
fn native_local_clocks_and_store_size_do_not_select_or_age_the_skip_reason_action() {
    let mut row = persistent();
    pvs(&mut row).pages_skipped_for_oldest_aborted_transaction = Some("4".into());
    for language in ["en", "es", "fr", "de"] {
        culprit_core::i18n::scope(language, || {
            let report = native_report(&[&row], &[source(&row.source, SourceState::Collected)]);
            assert_eq!(report.entries[0].investigation, t!("view.cleanup.pvs.investigate.aborted"));
            assert_eq!(report.entries[0].verify, t!("view.cleanup.pvs.verify"));
        });
    }
    let before = native_report(&[&row], &[source(&row.source, SourceState::Collected)]);
    let store = pvs(&mut row);
    store.off_row_kibibytes = Some("0".into());
    store.off_row_cleanup.started_without_offset = None;
    store.off_row_cleanup.ended_without_offset = None;
    let after = native_report(&[&row], &[source(&row.source, SourceState::Collected)]);
    assert_eq!(before.entries[0].investigation, after.entries[0].investigation);
    assert_eq!(before.entries[0].mechanisms, after.entries[0].mechanisms);
}
