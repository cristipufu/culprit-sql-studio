//! The existing bounded summary renderer consumes only the canonical runtime report.
use super::{
    SourceQualification, Summary, append_remaining_limitations, native_metrics, native_work_summary, statement,
};
use culprit_core::FullTextPopulationReport;

pub(crate) fn full_text_populations(report: &FullTextPopulationReport) -> String {
    let target = report.original.as_ref().map(|original| {
        let unknown = culprit_core::t!("view.full_text_populations.unknown");
        let schema = original.metadata.schema.as_deref().map(statement::preview).unwrap_or_else(|| unknown.clone());
        let table = original.metadata.table.as_deref().map(statement::preview).unwrap_or(unknown);
        let metrics = [culprit_core::Metric::said_text(
            culprit_core::key!("view.storage.field.observed_at"),
            original.metadata.observed_at.clone(),
        )];
        let mut details = native_metrics(&metrics).collect::<Vec<_>>();
        details.push(culprit_core::t!(
            "view.full_text_populations.target",
            database = culprit_core::format::digits(original.target.table.database_id.to_string()),
            table = culprit_core::format::digits(original.target.table.object_id.to_string()),
            catalog = culprit_core::format::digits(original.target.catalog_id.to_string()),
            index = culprit_core::format::digits(original.target.unique_index_id.to_string())
        ));
        (format!("{schema}.{table}"), details)
    });
    let mut text = native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single {
                state: Some(report.source.state),
                reason: report.source.reason.as_deref(),
            },
            limitations: &report.limitations,
        },
        target.as_ref().map(|(label, details)| (label.as_str(), details.clone())).into_iter().chain(
            report.entries.iter().map(|entry| {
                let mut details = native_metrics(&entry.metrics).collect::<Vec<_>>();
                details.extend([report.investigation.clone(), report.verify.clone()]);
                (entry.summary.as_str(), details)
            }),
        ),
    );
    append_remaining_limitations(&mut text, &report.limitations);
    text
}
#[cfg(test)]
mod tests;
