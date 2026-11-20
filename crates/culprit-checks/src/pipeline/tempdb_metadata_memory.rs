use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::TempdbMetadataMemoryReport>> {
    let read = snapshot.operations.tempdb_metadata_memory.as_deref()?;
    let result = crate::tempdb_metadata_memory::explain_captured(read).map(Box::new);
    admit_explanation(
        snapshot,
        culprit_core::source_id::TEMPDB_METADATA_MEMORY.into(),
        culprit_core::message!("view.tempdb_metadata_memory.invalid"),
        result,
    )
}
