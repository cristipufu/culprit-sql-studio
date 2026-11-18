use super::*;
pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::StorageReport>> {
    let result = crate::storage::AllocationEvidence::from_snapshot(snapshot)
        .and_then(|_| crate::storage::explain_snapshot(snapshot));
    match result {
        Ok(report) => report.map(Box::new),
        Err(error) => admit_explanation(
            snapshot,
            culprit_core::source_id::FILE_CAPACITY.into(),
            culprit_core::message!("view.storage.invalid"),
            Err(error),
        ),
    }
}
