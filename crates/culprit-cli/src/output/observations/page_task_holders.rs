//! Native endpoints and the final mapping stay independent; this renderer adds no relationship or verdict.
use super::{blocking::line, statement::sql, *};
use culprit_core::{
    page_wait_report::{PageTaskHolderExplanation, PageTaskHolderReport},
    telemetry::PageTaskHolderTargetCheck,
};

const MAX_HOLDER_LIMITATIONS: usize = 8;

pub(crate) fn page_task_holders(report: &PageTaskHolderReport) -> String {
    render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &[],
        },
        |text| {
            line(text, &t!("evidence.cli.page_holders.original_page", record = &report.original_page.record_id));
            for entry in report.entries.iter().take(super::super::BRIEF_FINDING_LIMIT) {
                point(text, entry);
            }
            match &report.final_target {
                Some(target) => final_mapping(text, target),
                None => line(text, &t!("evidence.cli.page_holders.mapping_unavailable")),
            }
            for limitation in report.limitations.iter().take(MAX_HOLDER_LIMITATIONS) {
                line(text, limitation);
            }
        },
    )
}

fn point(text: &mut String, entry: &PageTaskHolderExplanation) {
    line(text, &t!("evidence.cli.page_holders.point", title = &entry.title, at = &entry.observed_at));
    line(text, &entry.explanation);
    match &entry.task {
        Some(task) => line(
            text,
            &t!(
                "evidence.cli.page_holders.task",
                address = format!("{:?}", task.task_address),
                session = native_number(task.session_id),
                request = native_number(task.request_id),
                ecid = format::digits(task.execution_context_id.to_string())
            ),
        ),
        None => line(text, &t!("evidence.cli.page_holders.task_unavailable")),
    }
    match &entry.request {
        Some(request) => line(text, &super::blocking::request(request)),
        None => line(text, &t!("evidence.cli.page_holders.request_unavailable")),
    }
    line(text, &sql(entry.statement.as_ref()));
    line(text, &entry.investigation);
    line(text, &entry.verify);
}

fn final_mapping(text: &mut String, check: &PageTaskHolderTargetCheck) {
    let header = &check.target.header;
    line(
        text,
        &t!(
            "evidence.cli.page_holders.final_mapping",
            at = &check.observed_at,
            database = format::digits(header.database_id.to_string()),
            file = format::digits(header.file_id.to_string()),
            page = format::digits(header.page_id.to_string()),
            object = native_identifier(header.object_id.as_deref()),
            index = native_identifier(header.index_id.as_deref()),
            partition = native_identifier(header.partition_id.as_deref()),
            allocation = native_identifier(header.allocation_unit_id.as_deref())
        ),
    );
}

fn native_number(value: Option<u32>) -> String {
    value.map_or_else(|| t!("evidence.cli.page_holders.not_recorded"), |value| format::digits(value.to_string()))
}

fn native_identifier(value: Option<&str>) -> String {
    value.map_or_else(|| t!("evidence.cli.page_holders.not_recorded"), |value| format::digits(value.to_owned()))
}

#[cfg(test)]
mod tests;
