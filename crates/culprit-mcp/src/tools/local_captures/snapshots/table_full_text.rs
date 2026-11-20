use culprit_core::TableFullTextReport;
use serde_json::{Value, json};

pub(super) fn value(report: &TableFullTextReport) -> Value {
    json!({
        "title":report.title,"summary":report.summary,"source_state":report.source_state,
        "source_reason":report.source_reason,"read_complete":report.read_complete,
        "native_rows":report.native_rows,"observed_rows":report.observed_rows,
        "entries_available":report.entries.len(),"entries_presented":report.entries.len().min(super::ENTRY_LIMIT),
        "entries_limited":report.entries.len()>super::ENTRY_LIMIT,"limitations":report.limitations,
        "entries":report.entries.iter().take(super::ENTRY_LIMIT).map(|entry|json!({
            "schema":entry.schema,"table":entry.table,"summary":entry.summary,"metrics":entry.metrics,
            "qualification":entry.qualification,"verify":entry.verify,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests;
