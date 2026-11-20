//! Selected current catalog values and qualifications come from the canonical report, never a new scan.
use super::{SourceQualification, Summary, append_remaining_limitations, native_metrics, native_work_summary};
use culprit_core::ColumnstoreRowgroupReport;
pub(crate) fn columnstore_rowgroups(report: &ColumnstoreRowgroupReport) -> String {
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
        report.entries.iter().map(|entry| {
            let mut details = native_metrics(&entry.metrics).collect::<Vec<_>>();
            details.extend([report.investigation.clone(), report.verify.clone()]);
            (entry.title.as_str(), details)
        }),
    );
    append_remaining_limitations(&mut text, &report.limitations);
    text
}
#[cfg(test)]
mod tests;
