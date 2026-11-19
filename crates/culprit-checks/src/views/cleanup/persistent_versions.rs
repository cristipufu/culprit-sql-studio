use super::*;
use culprit_core::{CleanupMechanism, Metric};

struct RecordedSkip<'a> {
    pages: &'a str,
    mechanism: CleanupMechanism,
    explanation: &'static str,
    investigation: &'static str,
}

fn recorded_skips(store: &MssqlPersistentVersionStore) -> impl Iterator<Item = RecordedSkip<'_>> {
    [
        (
            &store.pages_skipped_for_secondaries,
            CleanupMechanism::SecondaryLowWatermark,
            culprit_core::key!("view.cleanup.pvs.secondary"),
            culprit_core::key!("view.cleanup.pvs.investigate.secondary"),
        ),
        (
            &store.pages_skipped_for_oldest_active_transaction,
            CleanupMechanism::OldestActiveTransaction,
            culprit_core::key!("view.cleanup.pvs.active"),
            culprit_core::key!("view.cleanup.pvs.investigate.active"),
        ),
        (
            &store.pages_skipped_for_long_snapshot_scan,
            CleanupMechanism::LongSnapshotScan,
            culprit_core::key!("view.cleanup.pvs.snapshot_scan"),
            culprit_core::key!("view.cleanup.pvs.investigate.snapshot_scan"),
        ),
        (
            &store.pages_skipped_for_online_index_rebuild,
            CleanupMechanism::OnlineIndexRebuild,
            culprit_core::key!("view.cleanup.pvs.online_rebuild"),
            culprit_core::key!("view.cleanup.pvs.investigate.online_rebuild"),
        ),
        (
            &store.pages_skipped_for_oldest_aborted_transaction,
            CleanupMechanism::OldestAbortedTransaction,
            culprit_core::key!("view.cleanup.pvs.aborted"),
            culprit_core::key!("view.cleanup.pvs.investigate.aborted"),
        ),
    ]
    .into_iter()
    .filter_map(|(pages, mechanism, explanation, investigation)| {
        let pages = pages.as_deref().filter(|pages| pages.parse::<u64>().is_ok_and(|value| value > 0))?;
        Some(RecordedSkip { pages, mechanism, explanation, investigation })
    })
}

pub(super) fn explain(row: &OperationalObservation, store: &MssqlPersistentVersionStore) -> CleanupExplanation {
    let mut mechanisms = vec![CleanupMechanism::PersistentVersionStore];
    let mut explanation = vec![t!("view.cleanup.pvs.off_row")];
    let mut investigation = Vec::new();
    for skip in recorded_skips(store) {
        mechanisms.push(skip.mechanism);
        explanation
            .push(culprit_core::i18n::t(skip.explanation, &[("pages", format::digits(skip.pages.to_owned()).into())]));
        investigation.push(culprit_core::i18n::t(skip.investigation, &[]));
    }
    if mechanisms.len() == 1 {
        explanation.push(t!("view.cleanup.pvs.no_recorded_cause"));
    }
    if store.pages_skipped_for_aborted_transactions.is_some() {
        explanation.push(t!("view.cleanup.pvs.threshold"));
    }
    let mut metrics = Vec::new();
    for (field, value) in [
        ("persistent_version_store_size_kb", &store.off_row_kibibytes),
        ("online_index_version_store_size_kb", &store.online_index_kibibytes),
        ("current_aborted_transaction_count", &store.aborted_transactions),
        ("oldest_active_transaction_id", &store.oldest_active_transaction_id),
        ("oldest_aborted_transaction_id", &store.oldest_aborted_transaction_id),
        ("min_transaction_timestamp", &store.minimum_transaction_timestamp),
        ("online_index_min_transaction_timestamp", &store.online_index_minimum_timestamp),
        ("secondary_low_water_mark", &store.secondary_low_water_mark),
        ("pvs_off_row_page_skipped_low_water_mark", &store.pages_skipped_for_secondaries),
        ("pvs_off_row_page_skipped_transaction_not_cleaned", &store.pages_skipped_for_aborted_transactions),
        ("pvs_off_row_page_skipped_oldest_active_xdesid", &store.pages_skipped_for_oldest_active_transaction),
        ("pvs_off_row_page_skipped_min_useful_xts", &store.pages_skipped_for_long_snapshot_scan),
        ("pvs_off_row_page_skipped_oldest_snapshot", &store.pages_skipped_for_online_index_rebuild),
        ("pvs_off_row_page_skipped_oldest_aborted_xdesid", &store.pages_skipped_for_oldest_aborted_transaction),
        ("offrow_version_cleaner_start_time", &store.off_row_cleanup.started_without_offset),
        ("offrow_version_cleaner_end_time", &store.off_row_cleanup.ended_without_offset),
        ("aborted_version_cleaner_start_time", &store.aborted_cleanup.started_without_offset),
        ("aborted_version_cleaner_end_time", &store.aborted_cleanup.ended_without_offset),
    ] {
        if let Some(value) = value {
            metrics.push(Metric::text(t!("view.cleanup.metric.native", field = field), value.clone()).keyed(field));
        }
    }
    CleanupExplanation {
        reference: native_reference(row),
        mechanisms,
        title: t!("view.cleanup.pvs.title"),
        explanation: explanation.join(" "),
        metrics,
        investigation: if investigation.is_empty() {
            t!("view.cleanup.pvs.investigate")
        } else {
            investigation.join(" ")
        },
        verify: t!("view.cleanup.pvs.verify"),
    }
}
