//! The shared report owns native meaning; this formatter bounds and escapes its six configuration fields.
use super::{SourceQualification, Summary, append_remaining_limitations, native_work_summary, statement};
use culprit_core::{PoolSettingsReport, t};
const FIELD_LIMIT: usize = 6;
const TEXT_LIMIT: usize = 1024;
fn text(value: &str) -> String {
    statement::preview_bounded(value, TEXT_LIMIT)
}
pub(crate) fn pool_settings(report: &PoolSettingsReport) -> String {
    let title = text(&report.title);
    let summary = text(&report.summary);
    let limitations = report.limitations.iter().take(8).map(|value| text(value)).collect::<Vec<_>>();
    let mut details = Vec::new();
    if let Some(target) = &report.elastic_pool {
        details.push(t!(
            "evidence.cli.replication_field",
            label = t!("view.pool_settings.pool"),
            value = text(&target.native.id)
        ));
    }
    if let Some(server) = &report.logical_server {
        details.push(t!(
            "evidence.cli.replication_field",
            label = t!("view.pool_settings.logical_server"),
            value = text(server)
        ));
    }
    if let Some(database) = &report.member_database {
        details.push(t!(
            "evidence.cli.replication_field",
            label = t!("view.pool_settings.database"),
            value = text(database)
        ));
    }
    for field in report.fields.iter().take(FIELD_LIMIT) {
        details.push(t!(
            "evidence.cli.replication_field",
            label = text(&field.label),
            value = field.value.as_deref().map(text).unwrap_or_else(|| t!("evidence.cli.replication_unknown"))
        ));
        details.push(text(&field.explanation));
    }
    if let Some(context) = report.context.as_ref().and_then(|context| context.native()) {
        details.push(t!(
            "evidence.cli.replication_bounds",
            from = text(&context.observed_from),
            until = text(&context.observed_until)
        ));
    }
    if let Some(at) = &report.observed_at {
        details.push(t!(
            "evidence.cli.replication_field",
            label = t!("view.pool_settings.native_clock"),
            value = text(at)
        ));
    }
    details.push(text(&report.investigation));
    details.push(text(&report.verify));
    let mut result = native_work_summary(
        Summary {
            title: &title,
            summary: &summary,
            source: SourceQualification::Single { state: Some(report.source_state), reason: None },
            limitations: &limitations,
        },
        std::iter::once((title.as_str(), details)),
    );
    append_remaining_limitations(&mut result, &limitations);
    result
}
pub(crate) fn compact(report: &PoolSettingsReport) -> serde_json::Value {
    serde_json::json!({"title":text(&report.title),"summary":text(&report.summary),"scope":"pool","source_state":report.source_state,
        "target_scope":report.target.as_ref().map(|target|match target { culprit_core::telemetry::CollectionTarget::Database(_) => "database", culprit_core::telemetry::CollectionTarget::Shared(_) => "shared" }),
        "elastic_pool_error":report.elastic_pool_error,"pool":report.elastic_pool.as_ref().map(|pool|text(&pool.native.id)),"member_database":report.member_database.as_deref().map(text),"logical_server":report.logical_server.as_deref().map(text),
        "native_rows":report.native_source.as_ref().map(|source|source.records),"observed_rows":report.observed_rows,"read_complete":report.read_complete,
        "observed_at":report.observed_at.as_deref().map(text),"fields_available":report.fields.len(),"fields_limited":report.fields.len()>FIELD_LIMIT,
        "fields":report.fields.iter().take(FIELD_LIMIT).map(|field|serde_json::json!({"key":text(&field.key),"label":text(&field.label),"native_value":field.native_value.as_deref().map(text),"unit":text(&field.unit),"value":field.value.as_deref().map(text),"explanation":text(&field.explanation)})).collect::<Vec<_>>(),
        "limitations":report.limitations.iter().take(8).map(|value|text(value)).collect::<Vec<_>>(),"investigation":text(&report.investigation),"verify":text(&report.verify)})
}
#[cfg(test)]
mod tests;
