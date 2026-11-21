//! Public derived configuration only; original member GUIDs/receipts are never projected from a raw read.
use culprit_core::{PoolSettingsReport, Report};
use serde_json::{Value, json};
const FIELD_LIMIT: usize = 6;
const TEXT_LIMIT: usize = 1024;
fn text(value: &str) -> String {
    let mut chars = value.chars();
    let mut value = chars.by_ref().take(TEXT_LIMIT).collect::<String>();
    if chars.next().is_some() {
        value.push('…');
    }
    value
}
pub(crate) fn value(report: &PoolSettingsReport) -> Value {
    json!({"title":text(&report.title),"summary":text(&report.summary),"scope":"pool","source_state":report.source_state,
        "target_scope":report.target.as_ref().map(|target|match target { culprit_core::telemetry::CollectionTarget::Database(_) => "database", culprit_core::telemetry::CollectionTarget::Shared(_) => "shared" }),
        "elastic_pool_error":report.elastic_pool_error,"pool":report.elastic_pool.as_ref().map(|pool|text(&pool.native.id)),"member_database":report.member_database.as_deref().map(text),"logical_server":report.logical_server.as_deref().map(text),
        "native_source":report.native_source.as_ref().map(|source|json!({"source":text(&source.source),"state":source.state,"records":source.records,"complete_scope":source.complete_scope,"error_code":source.error_code,"stopped":source.stopped})),
        "native_rows":report.native_source.as_ref().map(|source|source.records),"observed_rows":report.observed_rows,"read_complete":report.read_complete,
        "original_read_bounds":report.context.as_ref().and_then(|context|context.native()).map(|context|json!({"from":text(&context.observed_from),"until":text(&context.observed_until)})),
        "observed_at":report.observed_at.as_deref().map(text),"fields_available":report.fields.len(),"fields_limited":report.fields.len()>FIELD_LIMIT,
        "fields":report.fields.iter().take(FIELD_LIMIT).map(|field|json!({"key":text(&field.key),"label":text(&field.label),"native_value":field.native_value.as_deref().map(text),"unit":text(&field.unit),"value":field.value.as_deref().map(text),"explanation":text(&field.explanation)})).collect::<Vec<_>>(),
        "limitations":report.limitations.iter().take(8).map(|value|text(value)).collect::<Vec<_>>(),"investigation":text(&report.investigation),"verify":text(&report.verify)})
}
pub(super) fn attach(response: &mut Value, report: &Report) {
    if let Some(pool) = &report.pool_settings {
        response["pool_settings"] = value(pool);
    }
}
#[cfg(test)]
mod tests;
