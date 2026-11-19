//! A recorded incident is independent of current rows and retains only its authenticated original context.
use super::{SourceQualification, Summary, blocking::line, render_summary, request_labels, statement};
use culprit_core::{PageWaitReport, t};

pub(crate) fn page_waits(report: &PageWaitReport) -> String {
    let titles = report
        .entries
        .iter()
        .take(super::super::BRIEF_FINDING_LIMIT)
        .map(|entry| {
            t!(
                "evidence.cli.page_wait_entry",
                title = entry.title.as_str(),
                session = entry.request_key.session.id.as_str(),
                resource = entry.resource.native.id.as_str()
            )
        })
        .collect::<Vec<_>>();
    render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        |text| {
            if let Some(incident) = &report.recorded_incident {
                line(text, &t!("view.page_waits.recorded.title"));
                line(
                    text,
                    &t!(
                        "view.page_waits.recorded.context",
                        at = statement::preview(&incident.observed_at),
                        session = statement::preview(&incident.request.session.id),
                        request = request_labels::optional(
                            incident.request.request.as_ref().map(|request| request.id.as_str())
                        ),
                        resource = statement::preview(&incident.resource.native.id)
                    ),
                );
                line(text, &statement::sql(Some(&incident.sql)));
                if incident.client.is_some() {
                    line(text, &request_labels::client(incident.client.as_ref()));
                }
                line(text, &t!("view.page_waits.recorded.qualification"));
            }
            super::super::append_brief_titles(text, titles.iter().map(String::as_str));
        },
    )
}

#[cfg(test)]
mod tests;
