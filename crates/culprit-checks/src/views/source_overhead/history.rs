//! Historical acquisition summaries are reworded without joining any current database/workload identity.
use culprit_core::{
    HistoricalSourceOverhead, MAX_RECORDED_SOURCE_ACQUISITIONS, RecordedSourceAcquisition, Snapshot, format, message,
    source_acquisition_positions,
};

pub(super) fn explain(snapshot: &Snapshot) -> Vec<HistoricalSourceOverhead> {
    let originals = &snapshot.operations.source_acquisitions;
    let positions = source_acquisition_positions(originals);
    let count = positions.len();
    let mut reports = positions
        .into_iter()
        .take(MAX_RECORDED_SOURCE_ACQUISITIONS)
        .map(|position| {
            let Ok(position) = position else { return invalid() };
            match &originals[position] {
                RecordedSourceAcquisition::Unavailable { reason } => {
                    HistoricalSourceOverhead::Unavailable { reason: reason.render() }
                }
                RecordedSourceAcquisition::Captured { original } if original.is_saveable() => {
                    crate::source_overhead::explain_recorded(original)
                        .map(|report| HistoricalSourceOverhead::Available {
                            manifest_sha256: original.manifest_sha256.clone(),
                            report: Box::new(report),
                        })
                        .unwrap_or_else(|_| invalid())
                }
                _ => invalid(),
            }
        })
        .collect::<Vec<_>>();
    if count > MAX_RECORDED_SOURCE_ACQUISITIONS {
        reports.push(HistoricalSourceOverhead::Unavailable {
            reason: message!(
                "view.source_overhead.history_overflow",
                shown = format::digits(MAX_RECORDED_SOURCE_ACQUISITIONS.to_string()),
                recorded = format::digits(count.to_string())
            )
            .render(),
        });
    }
    reports
}
fn invalid() -> HistoricalSourceOverhead {
    HistoricalSourceOverhead::Unavailable { reason: message!("view.source_overhead.invalid_saved").render() }
}
#[cfg(test)]
mod tests;
