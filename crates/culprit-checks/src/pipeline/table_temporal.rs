use super::*;
pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::TableTemporalReport>> {
    match crate::table_temporal::explain_snapshot(snapshot) {
        Ok(report) => report.map(Box::new),
        Err(error) => admit_explanation(
            snapshot,
            culprit_core::source_id::TABLES.into(),
            culprit_core::message!("view.table_temporal.invalid"),
            Err(error),
        ),
    }
}
