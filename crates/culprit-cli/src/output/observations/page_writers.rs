//! Render the shared writer report without acquiring SQL or revising its original native relationships.
use super::{blocking::line, statement, *};
use culprit_core::{PageWaitWriterExplanation, PageWaitWriterReport, telemetry::MappedPageTarget};

const MAX_WRITER_LIMITATIONS: usize = 8;

pub(crate) fn page_wait_writers(report: &PageWaitWriterReport) -> String {
    let reason = report.source_reason.as_deref().map(statement::preview);
    render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: reason.as_deref() },
            limitations: &[],
        },
        |text| {
            line(text, &t!("evidence.cli.page_holders.original_page", record = &report.original_page.record_id));
            original_target(text, &report.original_target);
            line(
                text,
                &t!(
                    "evidence.cli.page_writers.counts",
                    shown = format::digits(report.entries.len().min(super::super::BRIEF_FINDING_LIMIT).to_string()),
                    total = format::digits(report.entries.len().to_string())
                ),
            );
            for entry in report.entries.iter().take(super::super::BRIEF_FINDING_LIMIT) {
                candidate(text, entry);
            }
            for limitation in report.limitations.iter().take(MAX_WRITER_LIMITATIONS) {
                line(text, limitation);
            }
        },
    )
}

fn original_target(text: &mut String, target: &MappedPageTarget) {
    let object = target.object.as_ref();
    line(
        text,
        &t!(
            "evidence.cli.page_writers.original_target",
            schema = label(object.map(|object| object.schema.as_str())),
            object = label(object.map(|object| object.name.as_str())),
            index = label(object.and_then(|object| object.index_name.as_deref())),
            database = format::digits(target.header.database_id.to_string()),
            file = format::digits(target.header.file_id.to_string()),
            page = format::digits(target.header.page_id.to_string())
        ),
    );
}

fn candidate(text: &mut String, entry: &PageWaitWriterExplanation) {
    line(
        text,
        &t!(
            "evidence.cli.observation_entry",
            title = statement::preview(&entry.title),
            explanation = &entry.explanation
        ),
    );
    line(text, &statement::sql(entry.sql.as_ref()));
    let client = entry.client.as_ref();
    line(
        text,
        &t!(
            "evidence.cli.page_writers.client",
            application = label(client.and_then(|client| client.application.as_deref())),
            user = label(client.and_then(|client| client.user.as_deref())),
            host = label(client.and_then(|client| client.host.as_deref()))
        ),
    );
    line(text, &entry.investigation);
    line(text, &entry.verify);
}

fn label(value: Option<&str>) -> String {
    value.map_or_else(|| t!("evidence.cli.page_holders.not_recorded"), statement::preview)
}

#[cfg(test)]
mod tests;
