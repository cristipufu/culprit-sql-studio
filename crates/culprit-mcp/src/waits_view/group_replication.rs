//! Bounded native values only; no original group/member/process identities or authenticated receipt internals.
use culprit_core::GroupReplicationReport;
use serde_json::{Value, json};
pub(crate) fn value(report: &GroupReplicationReport) -> Value {
    const ENTRY_LIMIT: usize = 5;
    let entries = report
        .entries
        .iter()
        .take(ENTRY_LIMIT)
        .map(|entry| {
            json!({
                "title":entry.title,"explanation":entry.explanation,
                "fields":GroupReplicationReport::public_fields(entry).collect::<Vec<_>>(),
                "groups":entry.groups,"investigation":entry.investigation,"verify":entry.verify,
            })
        })
        .collect::<Vec<_>>();
    json!({"title":report.title,"summary":report.summary,"sources":report.sources,"limitations":report.limitations,
        "read_complete":report.read_complete,"native_rows":report.native_rows,"observed_rows":report.observed_rows,
        "entries_available":report.entries.len(),"entries_presented":entries.len(),"entries_omitted":report.entries.len()-entries.len(),
        "entries_limited":report.entries_limited || report.entries.len()>entries.len(),"entries":entries,
        "original_read_bounds":report.original.as_ref().map(|original|json!({"from":original.context.observed_from,"until":original.context.observed_until}))})
}
pub(super) fn attach(value: &mut Value, report: &culprit_core::Report) {
    if let Some(reports) = &report.group_replication {
        value["group_replication"] = json!(reports.iter().map(self::value).collect::<Vec<_>>());
    }
}
