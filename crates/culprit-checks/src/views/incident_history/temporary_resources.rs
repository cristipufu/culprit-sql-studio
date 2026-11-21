//! Words one already authenticated answer; native facts and request ownership are established once in Core.
use super::*;

pub(super) fn explain(answer: &TemporaryResourceAnswer) -> Vec<String> {
    let mut reasons = vec![
        t!("view.tempdb_question.summary"),
        t!(
            "view.tempdb_question.clocks",
            original_from = &answer.basis_context.observed_from,
            original_until = &answer.basis_context.observed_until,
            result_from = &answer.result_context.observed_from,
            result_until = &answer.result_context.observed_until
        ),
        t!(
            "view.tempdb_question.coverage",
            state = match answer.source_state {
                SourceState::Collected => "collected",
                SourceState::Empty => "empty",
                SourceState::Partial => "partial",
                SourceState::Unavailable => "unavailable",
                SourceState::NotApplicable => "unsupported",
            }
        ),
        t!("view.tempdb_question.counters"),
        t!("view.tempdb_question.role"),
        t!(
            "view.tempdb_question.checked",
            n = answer.checked_task_context_count,
            count = format::digits(answer.checked_task_context_count.to_string())
        ),
        t!(
            "view.tempdb_question.receipts",
            n = answer.observed_tasks,
            count = format::digits(answer.observed_tasks.to_string())
        ),
    ];
    reasons.extend(answer.contributions.iter().map(contribution));
    reasons
}
fn contribution(value: &TemporaryResourceContribution) -> String {
    let state = match value {
        TemporaryResourceContribution::Observed { .. } => "observed",
        TemporaryResourceContribution::Unresolved { .. } => "unresolved",
    };
    match value.mechanism() {
        TemporaryResourceMechanism::UserAllocations => t!("view.tempdb_question.user", state = state),
        TemporaryResourceMechanism::InternalAllocations => t!("view.tempdb_question.internal", state = state),
        TemporaryResourceMechanism::CleanupBoundary => t!("view.tempdb_question.boundary", state = state),
        TemporaryResourceMechanism::VersionProtection => t!("view.tempdb_question.protection", state = state),
        TemporaryResourceMechanism::VersionGeneration => t!("view.tempdb_question.generation"),
        TemporaryResourceMechanism::SpillOrEstimate => t!("view.tempdb_question.spill"),
        TemporaryResourceMechanism::ConcurrentReservations => t!("view.tempdb_question.reservations"),
        TemporaryResourceMechanism::DataCapacity => t!("view.tempdb_question.data_capacity"),
        TemporaryResourceMechanism::LogCapacity => t!("view.tempdb_question.log_capacity"),
        TemporaryResourceMechanism::AllocationLatch => t!("view.tempdb_question.latch"),
        TemporaryResourceMechanism::PhysicalIo => t!("view.tempdb_question.io"),
    }
}

#[cfg(test)]
mod tests;
