//! Replay the preserved native admission facts without borrowing a workload population or the scan's host clock.
use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<culprit_core::CompilerAdmissionReport> {
    let captured = snapshot.operations.compiler_admission.as_ref()?;
    let source_id = captured.source.clone();
    let source = snapshot.sources.iter().find(|source| source.source == source_id);
    let result = if captured.observations.iter().all(|row| match &row.fact {
        culprit_core::telemetry::OperationalFact::CompilerWaiter(waiter) => {
            waiter.request.database_context_is_consistent(&snapshot.server.database)
        }
        _ => true,
    }) {
        crate::compiler_admission::explain(captured, source)
    } else {
        Err(culprit_core::Error::invalid(t!("view.compiler_admission.invalid")))
    };
    admit_explanation(snapshot, source_id, culprit_core::message!("view.compiler_admission.invalid"), result)
}
