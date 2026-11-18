use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::WaitCounterReport>> {
    let result = crate::wait_counters::explain_snapshot(snapshot);
    match result {
        Ok(report) => report.map(Box::new),
        Err(_) => {
            snapshot.record_unavailable_evidence(
                culprit_core::source_id::WAITS,
                culprit_core::message!("view.wait_counters.invalid"),
            );
            None
        }
    }
}
