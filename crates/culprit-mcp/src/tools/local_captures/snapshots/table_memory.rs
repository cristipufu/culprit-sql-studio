//! Only the local owner adds a stable authenticated entry reference to the same curated report projection.
use culprit_core::{Result, TableMemoryReport};
use culprit_engine::LocalSnapshotAnalysis;
use serde_json::{Value, json};
pub(super) fn value(analysis: &LocalSnapshotAnalysis, report: &TableMemoryReport) -> Result<Value> {
    let mut value = crate::waits_view::table_memory::value(report);
    if let Some(original) = &report.original
        && report.observed_rows == 1
    {
        value["entries"][0]["reference"] = json!(super::reference(analysis, &original.target.key()?));
    }
    Ok(value)
}
