use culprit_core::TableTemporalReport;
use serde_json::{Value, json};
pub(super) fn value(report: &TableTemporalReport) -> Value {
    let entries = report
        .entries
        .iter()
        .take(super::ENTRY_LIMIT)
        .map(|entry| json!({"schema":entry.schema,"table":entry.table,"fields":entry.fields}))
        .collect();
    let mut value = super::summary(
        &report.title,
        &report.summary,
        report.source_state,
        &report.source_reason,
        &report.limitations,
        report.entries.len(),
        entries,
    );
    value["qualification"] = json!(report.qualification);
    value["read_complete"] = json!(report.read_complete);
    value["native_rows"] = json!(report.native_rows);
    value["observed_rows"] = json!(report.observed_rows);
    value["observed_from"] = json!(report.observed_from);
    value["observed_until"] = json!(report.observed_until);
    value
}

#[cfg(test)]
mod tests;
