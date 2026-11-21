//! CDC text retains the shared bounded native log-capture renderer.
use culprit_core::CdcReport;
pub(crate) fn cdc(report: &CdcReport) -> String {
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
