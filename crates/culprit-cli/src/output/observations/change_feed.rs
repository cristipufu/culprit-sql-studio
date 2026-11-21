use culprit_core::ChangeFeedReport;
pub(crate) fn change_feed(report: &ChangeFeedReport) -> String {
    super::change_capture::render(
        &report.title,
        &report.summary,
        report.source_state,
        report.context.as_ref(),
        &report.limitations,
        &report.entries,
    )
}
#[cfg(test)]
mod tests;
