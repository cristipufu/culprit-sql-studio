//! Pure current-provider acquisition interpretation. It never attaches a historical source or native identity.
use culprit_core::{
    ProviderAcquisitionReport, ProviderAcquisitionSummary, ProviderReadCoverage, ProviderReadDisposition,
    ProviderReadKind, ProviderReaderExplanation, ProviderSetupAccounting, RecordedProviderAcquisition, format, t,
    telemetry::SourceAdmissionRoute,
};

pub fn explain_snapshot(snapshot: &culprit_core::Snapshot) -> Option<Box<ProviderAcquisitionReport>> {
    snapshot.operations.provider_acquisition.as_deref().map(|recorded| {
        Box::new(match recorded {
            RecordedProviderAcquisition::Available { summary } => {
                let route_matches = matches!(summary.route.as_ref(), Some(SourceAdmissionRoute::SavedConnection { name }) if name == &snapshot.connection);
                if summary.kind != ProviderReadKind::Scan || summary.engine != Some(snapshot.server.engine) || !route_matches {
                    ProviderAcquisitionReport::Unavailable { reason: t!("view.provider_acquisition.invalid") }
                } else {
                    explain(summary)
                }
            },
            RecordedProviderAcquisition::Unavailable { reason } => {
                ProviderAcquisitionReport::Unavailable { reason: reason.render() }
            }
        })
    })
}
pub fn explain(summary: &ProviderAcquisitionSummary) -> ProviderAcquisitionReport {
    if !summary.is_valid() {
        return ProviderAcquisitionReport::Unavailable { reason: t!("view.provider_acquisition.invalid") };
    }
    let settlement = match summary.native_disposition {
        ProviderReadDisposition::NotStarted => t!("view.provider_acquisition.native.not_started"),
        ProviderReadDisposition::Quiescent => t!("view.provider_acquisition.native.quiescent"),
        ProviderReadDisposition::Unconfirmed => t!("view.provider_acquisition.native.unconfirmed"),
    };
    let coverage = match &summary.coverage {
        ProviderReadCoverage::Complete => t!("view.provider_acquisition.coverage.complete"),
        ProviderReadCoverage::Partial { .. } => t!("view.provider_acquisition.coverage.partial"),
        ProviderReadCoverage::Unavailable { .. } => t!("view.provider_acquisition.coverage.unavailable"),
    };
    let readers = summary
        .readers
        .iter()
        .map(|reader| ProviderReaderExplanation {
            reader_id: reader.reader_id,
            work: super::source_overhead::explain_work(
                reader.work.as_deref(),
                super::source_overhead::WorkScope::Provider,
            ),
            setup_work: (reader.setup_accounting == ProviderSetupAccounting::Separate).then(|| {
                super::source_overhead::explain_work(
                    reader.setup_work.as_deref(),
                    super::source_overhead::WorkScope::Provider,
                )
            }),
        })
        .collect();
    let total_admissions = total(summary);
    ProviderAcquisitionReport::Available {
        evidence: Box::new(summary.clone()),
        title: t!("view.provider_acquisition.title"),
        summary: t!(
            "view.provider_acquisition.summary",
            elapsed = format::duration(summary.client_elapsed_ms),
            readers = format::count(summary.readers.len() as f64)
        ),
        settlement,
        coverage,
        total_admissions,
        readers,
        limitations: vec![
            t!("view.provider_acquisition.limit.client"),
            t!("view.provider_acquisition.limit.counts"),
            t!("view.provider_acquisition.limit.route"),
            t!("view.source_overhead.limit.preview"),
        ],
    }
}
fn total(summary: &ProviderAcquisitionSummary) -> Option<String> {
    if summary.coverage != ProviderReadCoverage::Complete {
        return None;
    }
    summary
        .readers
        .iter()
        .try_fold(0u64, |total, reader| {
            let total = total.checked_add(u64::from(reader.queries?))?;
            match reader.setup_accounting {
                ProviderSetupAccounting::IncludedInReaderWork | ProviderSetupAccounting::NotApplicable => Some(total),
                ProviderSetupAccounting::Separate => total.checked_add(u64::from(reader.setup_queries?)),
                ProviderSetupAccounting::Unknown => None,
            }
        })
        .map(|total| total.to_string())
}
#[cfg(test)]
mod tests;
