//! One bounded presentation policy for native CDC and change-feed original reports.
use super::{SourceQualification, Summary, append_remaining_limitations, native_work_summary, statement};
use culprit_core::{ChangeCaptureEntry, SourceState, t, telemetry::NativeReadContext};
const DISPLAY_FIELDS: usize = 5;
pub(super) fn render(
    title: &str,
    summary: &str,
    source_state: SourceState,
    context: Option<&NativeReadContext>,
    limitations: &[String],
    entries: &[ChangeCaptureEntry],
) -> String {
    let mut text = native_work_summary(
        Summary {
            title,
            summary,
            source: SourceQualification::Single { state: Some(source_state), reason: None },
            limitations,
        },
        entries.iter().map(|entry| {
            let mut details = vec![entry.explanation.clone()];
            details.extend(entry.fields.iter().take(DISPLAY_FIELDS).map(|field| {
                t!(
                    "view.counter_ratios.cli.metric",
                    label = field.label.clone(),
                    value = field
                        .value
                        .as_deref()
                        .map(statement::preview)
                        .unwrap_or_else(|| t!("view.wait_counters.unknown"))
                )
            }));
            details.extend([entry.investigation.clone(), entry.verify.clone()]);
            (entry.title.as_str(), details)
        }),
    );
    if let Some(context) = context {
        text.push('\n');
        text.push_str(&t!("view.wait_counters.bounds", from = &context.observed_from, until = &context.observed_until));
    }
    append_remaining_limitations(&mut text, limitations);
    text
}

pub(crate) fn compact(
    (title, summary): (&str, &str),
    state: SourceState,
    read_complete: bool,
    context: Option<&NativeReadContext>,
    limitations: &[String],
    entries: &[ChangeCaptureEntry],
    sources: &[culprit_core::telemetry::StateSourceCoverage],
) -> serde_json::Value {
    let displayed=entries.iter().take(super::super::BRIEF_FINDING_LIMIT).map(|entry|serde_json::json!({
        "title":entry.title,"explanation":entry.explanation,"observed_at":entry.observed_at,
        "fields":entry.fields.iter().take(DISPLAY_FIELDS).collect::<Vec<_>>(),"investigation":entry.investigation,"verify":entry.verify
    })).collect::<Vec<_>>();
    serde_json::json!({"title":title,"summary":summary,"source_state":state,"sources":sources,"read_complete":read_complete,"limitations":limitations,
        "entries_available":entries.len(),"entries_presented":displayed.len(),"entries":displayed,
        "original_read_bounds":context.map(|context|serde_json::json!({"from":context.observed_from,"until":context.observed_until}))})
}
