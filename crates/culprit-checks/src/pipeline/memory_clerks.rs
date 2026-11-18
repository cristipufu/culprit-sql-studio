use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::MemoryClerkReport>> {
    match crate::memory_clerks::explain_snapshot(snapshot) {
        Ok(report) => report.map(Box::new),
        Err(error) => admit_explanation(
            snapshot,
            culprit_core::source_id::MEMORY_CLERKS.into(),
            culprit_core::message!("collected.memory_clerks.invalid"),
            Err(error),
        ),
    }
}
