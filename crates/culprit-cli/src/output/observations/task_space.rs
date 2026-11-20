//! Display the pure view's admitted current request; a raw task row never lends SQL to this renderer.
use super::{SourceQualification, Summary, native_work_summary, request_labels, statement};
use culprit_core::{TaskSpaceReport, t};

pub(crate) fn task_space(report: &TaskSpaceReport) -> String {
    native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| {
            let mut fields = vec![entry.explanation.clone()];
            if let Some(request) = &entry.current_request {
                fields.push(t!("view.task_space.current_request.title"));
                fields.push(t!(
                    "view.task_space.current_request.interval",
                    from = statement::preview(&request.observed_from),
                    until = statement::preview(&request.observed_until)
                ));
                fields.push(t!("view.task_space.current_request.qualification"));
                fields.push(statement::sql(Some(&request.statement)));
                if request.client.is_some() {
                    fields.push(request_labels::client(request.client.as_ref()));
                }
            }
            fields.push(entry.investigation.clone());
            fields.push(entry.verify.clone());
            (entry.title.as_str(), fields)
        }),
    )
}
