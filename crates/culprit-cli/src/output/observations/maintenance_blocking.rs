//! The shared composition owns every relationship. Text keeps its historical and later native points separate.
use super::*;
use culprit_core::MaintenanceBlockingReport;

const MAX_COMPOSITION_LIMITATIONS: usize = super::super::BRIEF_FINDING_LIMIT;

pub(crate) fn maintenance_blocking(report: &MaintenanceBlockingReport) -> String {
    let mut text = format!("{}\n{}", report.title, report.summary);
    super::super::append_brief_details(
        &mut text,
        report.limitations.iter().take(MAX_COMPOSITION_LIMITATIONS).map(String::as_str),
        OBSERVATION_DETAIL_CHARS,
    );
    section(&mut text, &t!("evidence.cli.maintenance_blocking.historical"), &maintenance_progress(&report.maintenance));
    text.push('\n');
    text.push_str(&t!(
        "evidence.cli.maintenance_blocking.progress_bounds",
        source = &report.maintenance.source,
        from = &report.maintenance.context.observed_from,
        until = &report.maintenance.context.observed_until,
    ));
    text.push('\n');
    text.push_str(&t!(
        "evidence.cli.maintenance_blocking.progress_counts",
        native = format::digits(report.maintenance.native_rows.to_string()),
        observed = format::digits(report.maintenance.observed_rows.to_string()),
    ));
    if let Some(reference) = &report.maintenance_observation {
        text.push('\n');
        text.push_str(&t!("evidence.cli.maintenance_blocking.progress_receipt", record = &reference.record_id));
    }
    super::super::append_brief_details(
        &mut text,
        report
            .maintenance
            .entries
            .iter()
            .take(super::super::BRIEF_FINDING_LIMIT)
            .flat_map(|entry| [entry.explanation.as_str(), entry.investigation.as_str(), entry.verify.as_str()]),
        OBSERVATION_DETAIL_CHARS,
    );
    if !report.maintenance.read_complete {
        text.push('\n');
        text.push_str(&t!("evidence.cli.read_incomplete"));
    }
    section(&mut text, &t!("evidence.cli.maintenance_blocking.later"), &blocking(&report.blocking));
    text
}

fn section(text: &mut String, title: &str, body: &str) {
    text.push_str("\n\n");
    text.push_str(title);
    text.push('\n');
    text.push_str(body);
}

#[cfg(test)]
mod tests;
