//! A recorded incident is independent of current rows and retains only its authenticated original context.
use super::{SourceQualification, Summary, blocking::line, render_summary, request_labels, statement};
use culprit_core::{
    PageWaitExplanation, PageWaitRecordedIncident, PageWaitReport, t,
    telemetry::{ActivityClient, DefinitionPayload},
};

pub(crate) fn page_waits(report: &PageWaitReport) -> String {
    render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        |text| {
            if let Some(incident) = &report.recorded_incident {
                recorded_incident(text, incident);
            }
            for entry in report.entries.iter().take(super::super::BRIEF_FINDING_LIMIT) {
                append_entry(text, entry);
            }
        },
    )
}

fn recorded_incident(text: &mut String, incident: &PageWaitRecordedIncident) {
    line(text, &t!("view.page_waits.recorded.title"));
    line(
        text,
        &t!(
            "view.page_waits.recorded.context",
            at = statement::preview(&incident.observed_at),
            session = statement::preview(&incident.request.session.id),
            request = request_labels::optional(incident.request.request.as_ref().map(|request| request.id.as_str())),
            resource = statement::preview(&incident.resource.native.id)
        ),
    );
    append_statement_context(text, &incident.sql, incident.client.as_ref());
    line(text, &t!("view.page_waits.recorded.qualification"));
}

fn append_entry(text: &mut String, entry: &PageWaitExplanation) {
    let title = t!(
        "evidence.cli.page_wait_entry",
        title = entry.title.as_str(),
        session = entry.request_key.session.id.as_str(),
        resource = entry.resource.native.id.as_str()
    );
    let Some(request) = &entry.recorded_request else {
        super::super::append_brief_titles(text, std::iter::once(title.as_str()));
        return;
    };
    let mut detail = statement::preview(&title);
    line(&mut detail, &t!("view.page_waits.recorded_request.title"));
    line(
        &mut detail,
        &t!(
            "view.page_waits.recorded_request.context",
            at = statement::preview(&request.observed_at),
            session = statement::preview(&request.request.session.id),
            request = request_labels::optional(request.request.request.as_ref().map(|request| request.id.as_str()))
        ),
    );
    // Qualify the earlier context before any optional client text can reach the common output ceiling.
    line(&mut detail, &t!("view.page_waits.recorded_request.qualification"));
    append_statement_context(&mut detail, &request.sql, request.client.as_ref());
    super::super::append_brief_details(text, std::iter::once(detail.as_str()), super::OBSERVATION_DETAIL_CHARS);
}

fn append_statement_context(text: &mut String, sql: &DefinitionPayload, client: Option<&ActivityClient>) {
    line(text, &statement::sql(Some(sql)));
    if client.is_some() {
        line(text, &request_labels::client(client));
    }
}

#[cfg(test)]
mod tests;
