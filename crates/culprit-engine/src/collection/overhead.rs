//! Local acquisition accounting needs neither source configuration nor native/warehouse access.
use culprit_collector::outbox::Outbox;
use culprit_core::{Result, SourceOverheadEvidence, SourceOverheadReport, t};
use std::path::Path;

/// Inspect one allocated local attempt, including failure before source description or unsettled native work.
/// The bounded manifest is checked against the read-only journal; native result files are not verified/read.
/// No manifest means no original acquisition clock is available, so no evidence is invented for that allocation.
pub async fn inspect_source_overhead(directory: &Path, observation_id: &str) -> Result<Option<SourceOverheadReport>> {
    let Some(manifest) = Outbox::inspect_manifest(directory, observation_id).await? else {
        return Ok(None);
    };
    let evidence = SourceOverheadEvidence::from_manifest(&manifest);
    let mut report = culprit_checks::source_overhead::explain(&evidence)?;
    report.limitations.push(t!("view.source_overhead.limit.local"));
    Ok(Some(report))
}

#[cfg(test)]
mod tests;
