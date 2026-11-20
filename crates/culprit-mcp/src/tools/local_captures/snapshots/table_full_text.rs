use culprit_core::TableFullTextReport;
use serde_json::{Value, json};

pub(super) fn value(report: &TableFullTextReport) -> Value {
    let entries = report
        .entries
        .iter()
        .take(super::ENTRY_LIMIT)
        .map(|entry| {
            json!({
                "schema":entry.schema,"table":entry.table,"summary":entry.summary,"metrics":entry.metrics,
                "qualification":entry.qualification,"verify":entry.verify,
            })
        })
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
    value["read_complete"] = json!(report.read_complete);
    value["native_rows"] = json!(report.native_rows);
    value["observed_rows"] = json!(report.observed_rows);
    value
}

#[cfg(test)]
mod tests;
