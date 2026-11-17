//! A bounded human view of the same admitted report; identifiers remain historical and SQL stays unavailable.
use super::*;
use culprit_core::page_server_read_report::{PageServerReadEntryIdentity, PageServerReadReport};

pub(crate) fn page_server_reads(report: &PageServerReadReport) -> String {
    let mut text = native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| {
            let mut details = vec![
                entry.explanation.summary.clone(),
                t!("evidence.cli.page_server_reads.executions", count = format::digits(entry.executions.clone())),
            ];
            details.extend(native_metrics(&entry.explanation.metrics));
            details.push(historical_reference(&entry.identity));
            if let PageServerReadEntryIdentity::RuntimeFragment { reference, .. } = &entry.identity
                && let culprit_core::telemetry::CapturedRecordReference::Informational { position } = reference.as_ref()
            {
                details.push(t!(
                    "evidence.cli.page_server_reads.position",
                    stream = position.stream.file_name(),
                    row = format::digits(position.row_ordinal.to_string())
                ));
            }
            if let Some(statement) = &entry.statement {
                details.push(statement.explanation.clone());
                if let Some(preview) = &statement.preview {
                    details.push(t!("evidence.cli.page_server_reads.statement", statement = preview));
                }
                if statement.preview_truncated {
                    details.push(t!("view.query_store_statement.shortened"));
                }
                if let Some(unavailable) = &statement.unavailable_explanation {
                    details.push(unavailable.clone());
                }
            }
            details.extend(entry.explanation.limitations.iter().cloned());
            details.extend([entry.explanation.investigate.clone(), entry.explanation.verify.clone()]);
            (entry.explanation.title.as_str(), details)
        }),
    );
    append_remaining_limitations(&mut text, &report.limitations);
    text.push('\n');
    text.push_str(&t!(
        "evidence.cli.page_server_reads.counts",
        observed = format::digits(report.observed_rows.to_string()),
        selected = report
            .selected_rows
            .map(|count| format::digits(count.to_string()))
            .unwrap_or_else(|| t!("view.page_server_reads.unknown")),
        printed = format::digits(report.entries.len().min(super::super::BRIEF_FINDING_LIMIT).to_string())
    ));
    text.push('\n');
    if let Some(read) = &report.definition_read {
        text.push_str(&t!(
            "evidence.cli.page_server_reads.definition_counts",
            observed = format::digits(read.observed_rows.to_string()),
            selected = format::digits(read.selected_rows.to_string())
        ));
    } else {
        text.push_str(&t!("view.page_server_reads.limit.definition_link"));
    }
    text
}

pub(crate) fn collected_page_server_reads(
    analysis: &culprit_core::page_server_read_report::CollectedPageServerReadsAnalysis,
) -> String {
    let mut text = page_server_reads(&analysis.report);
    text.push('\n');
    text.push_str(&t!(
        "evidence.cli.page_server_reads.runtime_bounds",
        from = &analysis.context.collected_from,
        until = &analysis.context.collected_until
    ));
    if let Some(context) = &analysis.definition_context {
        text.push('\n');
        text.push_str(&t!(
            "evidence.cli.page_server_reads.definition_bounds",
            from = &context.collected_from,
            until = &context.collected_until
        ));
    }
    text
}

fn historical_reference(identity: &PageServerReadEntryIdentity) -> String {
    let unknown = t!("view.page_server_reads.unknown");
    let (query, plan, row) = match identity {
        PageServerReadEntryIdentity::RuntimeFragment { native, .. } => (
            native.references.query.as_ref().map(|query| query.native.id.as_str()),
            native.references.plan.as_ref().map(|plan| plan.native.id.as_str()),
            Some(native.native.id.as_str()),
        ),
        PageServerReadEntryIdentity::RecordedPlan { native_plan, native_fragment, .. } => (
            native_plan.as_ref().map(|plan| plan.query.query_id.as_str()),
            native_plan.as_ref().map(|plan| plan.plan_id.as_str()),
            native_fragment.as_ref().map(|row| row.id.as_str()),
        ),
    };
    t!(
        "evidence.cli.page_server_reads.reference",
        query = query.unwrap_or(&unknown),
        plan = plan.unwrap_or(&unknown),
        row = row.unwrap_or(&unknown)
    )
}

#[cfg(test)]
mod informational_tests;
#[cfg(test)]
mod tests;
