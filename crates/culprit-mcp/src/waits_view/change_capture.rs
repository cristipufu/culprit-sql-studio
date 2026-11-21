//! Canonical saved histories remain separate; this bounded projection supplies no current-DB join or new reference.
use culprit_core::{ChangeCaptureEntry, SourceState, telemetry::NativeReadContext};
use serde_json::{Value, json};
const ENTRIES: usize = 5;
const FIELDS: usize = 8;
fn value(
    (title, summary): (&str, &str),
    state: SourceState,
    complete: bool,
    context: Option<&NativeReadContext>,
    limitations: &[String],
    entries: &[ChangeCaptureEntry],
    sources: &[culprit_core::telemetry::StateSourceCoverage],
) -> Value {
    let displayed=entries.iter().take(ENTRIES).map(|entry|json!({"title":entry.title,"explanation":entry.explanation,
        "kind":entry.kind,"observed_at":entry.observed_at,"fields_available":entry.fields.len(),"fields":entry.fields.iter().take(FIELDS).collect::<Vec<_>>(),
        "investigation":entry.investigation,"verify":entry.verify})).collect::<Vec<_>>();
    json!({"title":title,"summary":summary,"source_state":state,"sources":sources,"read_complete":complete,"limitations":limitations,
        "entries_available":entries.len(),"entries_presented":displayed.len(),"entries_omitted":entries.len()-displayed.len(),"entries":displayed,
        "historical_context":true,"original_read_bounds":context.map(|context|json!({"from":context.observed_from,"until":context.observed_until}))})
}
pub(super) fn attach(response: &mut Value, report: &culprit_core::Report) {
    if let Some(reports) = &report.cdc {
        response["cdc"] = json!(
            reports
                .iter()
                .map(|report| value(
                    (&report.title, &report.summary),
                    report.source_state,
                    report.read_complete,
                    report.context.as_ref(),
                    &report.limitations,
                    &report.entries,
                    &report.sources
                ))
                .collect::<Vec<_>>()
        );
    }
    if let Some(reports) = &report.change_feed {
        response["change_feed"] = json!(
            reports
                .iter()
                .map(|report| value(
                    (&report.title, &report.summary),
                    report.source_state,
                    report.read_complete,
                    report.context.as_ref(),
                    &report.limitations,
                    &report.entries,
                    &report.sources
                ))
                .collect::<Vec<_>>()
        );
    }
}
#[cfg(test)]
mod tests;
