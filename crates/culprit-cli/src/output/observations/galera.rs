//! Native node evidence uses the existing bounded escaped observation formatter.
use super::{SourceQualification, Summary, append_remaining_limitations, native_work_summary, statement};
use culprit_core::{GaleraReport, t};
pub(crate) fn galera(report: &GaleraReport) -> String {
    let mut text = native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Multiple(&report.sources),
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| {
            let mut details = vec![entry.explanation.clone()];
            details.extend(
                entry.groups.iter().flat_map(|group| &group.fields).chain(GaleraReport::public_fields(entry)).map(
                    |field| {
                        t!(
                            "evidence.cli.replication_field",
                            label = &field.label,
                            value = field
                                .value
                                .as_deref()
                                .map(statement::preview)
                                .unwrap_or_else(|| t!("evidence.cli.replication_unknown"))
                        )
                    },
                ),
            );
            details.push(entry.investigation.clone());
            details.push(entry.verify.clone());
            (entry.title.as_str(), details)
        }),
    );
    if let Some(original) = &report.original {
        text.push('\n');
        text.push_str(&t!(
            "evidence.cli.replication_bounds",
            from = &original.context.observed_from,
            until = &original.context.observed_until
        ));
    }
    append_remaining_limitations(&mut text, &report.limitations);
    text
}
pub(crate) fn compact(report: &GaleraReport) -> serde_json::Value {
    serde_json::json!({"title":report.title,"summary":report.summary,"sources":report.sources,
        "limitations":report.limitations,"read_complete":report.read_complete,"native_rows":report.native_rows,
        "observed_rows":report.observed_rows,"presented_rows":report.presented_rows,"entries_limited":report.entries_limited,
        "original_read_bounds":report.original.as_ref().map(|original| serde_json::json!({"from":original.context.observed_from,"until":original.context.observed_until})),
        "entries":report.entries.iter().map(|entry|serde_json::json!({"title":entry.title,"explanation":entry.explanation,
            "fields":GaleraReport::public_fields(entry).collect::<Vec<_>>(),
            "groups":entry.groups,"investigation":entry.investigation,"verify":entry.verify})).collect::<Vec<_>>()})
}
#[cfg(test)]
mod tests;
