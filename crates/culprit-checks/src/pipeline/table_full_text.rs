use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::TableFullTextReport>> {
    match crate::table_full_text::explain_snapshot(snapshot) {
        Ok(report) => report.map(Box::new),
        Err(error) => admit_explanation(
            snapshot,
            culprit_core::source_id::TABLES.into(),
            culprit_core::message!("view.table_full_text.invalid"),
            Err(error),
        ),
    }
}
