//! Historical guidance has no executable current-session action; terminal data uses the common escaped preview.
use culprit_core::{IncidentHistoryReport, format, t};
const ENTRY_LIMIT: usize = 10;
pub(crate) fn incident_history(report: &IncidentHistoryReport) -> String {
    let mut text = format!("{}\n{}", report.title, report.summary);
    for entry in report.entries.iter().take(ENTRY_LIMIT) {
        text.push_str(&format!(
            "\n\n{} · {} · {}\n{}\n{}\n{}",
            entry.title,
            entry.phase,
            entry.scope_label,
            t!(
                "view.incident_history.cli.identity",
                episode = &entry.episode_id,
                controller = format::digits(entry.controller_version.to_string()),
                scope = &entry.scope_id,
                source = super::statement::preview(&entry.source)
            ),
            t!(
                "view.incident_history.cli.observed",
                first = &entry.first_seen_at,
                latest = &entry.last_seen_at,
                recorded = &entry.recorded_at
            ),
            entry.next_step
        ));
        if let Some(recipe) = &entry.followup_recipe {
            text.push('\n');
            text.push_str(&t!(
                "view.incident_history.cli.recipe",
                recipe = super::statement::preview(recipe),
                version = entry
                    .followup_recipe_version
                    .map(|version| format::digits(version.to_string()))
                    .unwrap_or_else(|| t!("ui.memory_failures.not_reported"))
            ));
        }
        for reason in &entry.reasons {
            text.push('\n');
            text.push_str(reason);
        }
        for evidence in &entry.evidence {
            text.push('\n');
            text.push_str(&t!(
                "view.incident_history.cli.evidence",
                label = &evidence.label,
                capture = super::statement::preview(&evidence.capture.capture.observation_id),
                availability = &evidence.availability
            ));
        }
    }
    if report.entries.len() > ENTRY_LIMIT {
        text.push('\n');
        text.push_str(&t!(
            "view.incident_history.cli.omitted",
            shown = format::digits(ENTRY_LIMIT.to_string()),
            total = format::digits(report.entries.len().to_string())
        ));
    }
    for limitation in &report.limitations {
        text.push('\n');
        text.push_str(limitation);
    }
    text
}
#[cfg(test)]
mod tests;
