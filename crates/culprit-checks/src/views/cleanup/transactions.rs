use super::*;
use culprit_core::{CleanupMechanism, Metric};

pub(super) fn explain(row: &OperationalObservation, horizon: &MssqlCleanupHorizon) -> CleanupExplanation {
    let mut mechanisms = Vec::new();
    let mut explanation = Vec::new();
    let mut metrics = Vec::new();
    let mut investigation = vec![t!("view.cleanup.transaction.investigate")];
    if let Some(transaction) = &horizon.oldest_confirmed {
        let transaction_context = DatabaseTransactionContext::classify(transaction);
        mechanisms.push(CleanupMechanism::ConfirmedDatabaseTransaction);
        explanation.push(t!("view.cleanup.transaction.confirmed"));
        explanation.push(transaction_context.explanation());
        investigation.push(transaction_context.investigation());
        context(transaction, TransactionRole::Confirmed, &mut metrics);
    }
    if let Some(transaction) = &horizon.oldest_unattributed_versioning {
        mechanisms.push(CleanupMechanism::UnattributedVersioning);
        explanation.push(t!("view.cleanup.transaction.unattributed"));
        context(transaction, TransactionRole::Unattributed, &mut metrics);
    }
    if mechanisms.is_empty() {
        explanation.push(t!("view.cleanup.transaction.empty"));
    }
    CleanupExplanation {
        reference: native_reference(row),
        mechanisms,
        title: t!("view.cleanup.transaction.title"),
        explanation: explanation.join(" "),
        metrics,
        investigation: investigation.join(" "),
        verify: t!("view.cleanup.transaction.verify"),
    }
}

#[derive(Clone, Copy)]
enum DatabaseTransactionContext {
    SnapshotWithoutLoggedWork,
    SnapshotWithLoggedWork,
    SnapshotWithUnknownLoggedWork,
    LoggedVersioningParticipant,
    LoggedWork,
}

impl DatabaseTransactionContext {
    /// The caller has validated this original row's database qualification and native membership.
    fn classify(transaction: &MssqlCleanupTransaction) -> Self {
        if transaction.snapshot_isolation == Some(true) {
            match transaction.log_record_count.as_deref().and_then(|value| value.parse::<u64>().ok()) {
                Some(0) => Self::SnapshotWithoutLoggedWork,
                Some(_) => Self::SnapshotWithLoggedWork,
                None => Self::SnapshotWithUnknownLoggedWork,
            }
        } else if transaction.row_version_participant {
            Self::LoggedVersioningParticipant
        } else {
            Self::LoggedWork
        }
    }

    fn explanation(self) -> String {
        match self {
            Self::SnapshotWithoutLoggedWork => t!("view.cleanup.transaction.context.snapshot_zero_log"),
            Self::SnapshotWithLoggedWork => t!("view.cleanup.transaction.context.snapshot_logged"),
            Self::SnapshotWithUnknownLoggedWork => t!("view.cleanup.transaction.context.snapshot_unknown_log"),
            Self::LoggedVersioningParticipant => t!("view.cleanup.transaction.context.logged_versioning"),
            Self::LoggedWork => t!("view.cleanup.transaction.context.logged"),
        }
    }

    fn investigation(self) -> String {
        match self {
            Self::SnapshotWithoutLoggedWork | Self::SnapshotWithUnknownLoggedWork => {
                t!("view.cleanup.transaction.investigate_snapshot")
            }
            Self::SnapshotWithLoggedWork => t!("view.cleanup.transaction.investigate_snapshot_logged"),
            Self::LoggedVersioningParticipant | Self::LoggedWork => {
                t!("view.cleanup.transaction.investigate_logged")
            }
        }
    }
}

#[derive(Clone, Copy)]
enum TransactionRole {
    Confirmed,
    Unattributed,
}

impl TransactionRole {
    fn prefix(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Unattributed => "unattributed",
        }
    }
    fn label(self) -> String {
        match self {
            Self::Confirmed => t!("view.cleanup.transaction.confirmed_role"),
            Self::Unattributed => t!("view.cleanup.transaction.unattributed_role"),
        }
    }
}

fn context(transaction: &MssqlCleanupTransaction, role: TransactionRole, metrics: &mut Vec<Metric>) {
    let role_label = role.label();
    for (field, value) in [
        ("session_id", Some(&transaction.session.id)),
        ("session_generation", Some(&transaction.session.generation)),
        ("transaction_id", Some(&transaction.transaction.id)),
        ("transaction_generation", Some(&transaction.transaction.generation)),
        ("observed_time_without_offset", Some(&transaction.observed_time_without_offset)),
        ("transaction_age_seconds", transaction.age_seconds.as_ref()),
        ("log_record_count", transaction.log_record_count.as_ref()),
        ("log_bytes_used", transaction.log_bytes_used.as_ref()),
        ("begin_lsn", transaction.begin_lsn.as_ref()),
        ("transaction_sequence_number", transaction.transaction_sequence_number.as_ref()),
        ("versioning_age_seconds", transaction.versioning_age_seconds.as_ref()),
    ] {
        if let Some(value) = value {
            metrics.push(
                Metric::text(t!("view.cleanup.metric.transaction", role = &role_label, field = field), value.clone())
                    .keyed(&format!("{}:{field}", role.prefix())),
            );
        }
    }
}
