use super::*;
use culprit_core::{CleanupMechanism, Metric};

pub(super) fn explain(row: &OperationalObservation, horizon: &MssqlCleanupHorizon) -> CleanupExplanation {
    let mut mechanisms = Vec::new();
    let mut explanation = Vec::new();
    let mut metrics = Vec::new();
    if let Some(transaction) = &horizon.oldest_confirmed {
        mechanisms.push(CleanupMechanism::ConfirmedDatabaseTransaction);
        explanation.push(t!("view.cleanup.transaction.confirmed"));
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
        investigation: t!("view.cleanup.transaction.investigate"),
        verify: t!("view.cleanup.transaction.verify"),
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
