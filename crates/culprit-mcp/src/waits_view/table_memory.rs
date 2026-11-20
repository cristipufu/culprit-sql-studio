//! Curated canonical historical values, without raw original manifests or native scope/proof payloads.
use culprit_core::TableMemoryReport;
use serde_json::{Value, json};
pub(crate) fn value(report: &TableMemoryReport) -> Value {
    let original = report.original.as_ref();
    let entries = if report.observed_rows == 1 {
        vec![json!({"observed_at":original.map(|original|&original.metadata.observed_at),"metrics":report.metrics})]
    } else {
        vec![]
    };
    json!({"title":report.title,"summary":report.summary,"source_state":report.source.state,"source_reason":report.source.reason,
        "limitations":report.limitations,"read_complete":report.read_complete,"native_rows":report.native_rows,"observed_rows":report.observed_rows,
        "entries_available":entries.len(),"entries_presented":entries.len(),"entries_omitted":0,"entries_limited":false,"entries":entries,
        "selected_target":original.map(|original|json!({"database_id":original.target.database_id,"object_id":original.target.object_id,"schema":original.metadata.schema,"table":original.metadata.name})),
        "original_read_bounds":original.map(|original|json!({"from":original.context.observed_from,"until":original.context.observed_until})),
        "native_availability":original.map(|original|json!({"observed_at":original.metadata.observed_at,"memory_optimized":original.metadata.memory_optimized,"xtp_supported":original.metadata.xtp_supported,"dmv_permission":original.metadata.dmv_permission,"metadata_visible":original.metadata.metadata_visible,"layout_available":original.metadata.layout_available}))})
}
pub(super) fn attach(response: &mut Value, report: &culprit_core::Report) {
    if let Some(reports) = &report.table_memory {
        response["table_memory"] = json!(reports.iter().map(value).collect::<Vec<_>>());
    }
}
