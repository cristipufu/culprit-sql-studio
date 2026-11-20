//! Backup history keeps the shared retained-metadata interpretation and bounded observation output.
use super::{SourceQualification, Summary, append_remaining_limitations, native_work_summary};
use culprit_core::BackupHistoryReport;
pub(crate) fn backup_history(report: &BackupHistoryReport) -> String {
    let mut text = native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: Some(report.source_state), reason: None },
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| {
            (entry.title.as_str(), vec![entry.explanation.clone(), entry.investigation.clone(), entry.verify.clone()])
        }),
    );
    append_remaining_limitations(&mut text, &report.limitations);
    text
}
#[cfg(test)]
mod tests;
