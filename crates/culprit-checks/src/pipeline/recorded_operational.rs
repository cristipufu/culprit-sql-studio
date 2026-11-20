//! One bounded interpretation of the shared replay policy; families own admission and every message.
use culprit_core::{
    Result,
    i18n::Message,
    telemetry::{MAX_RECORDED_OPERATIONAL_READS, RecordedOperationalRead},
};
pub(super) fn explain<R>(
    originals: &[RecordedOperationalRead],
    interpret: fn(&RecordedOperationalRead) -> Result<R>,
    unavailable: fn(&Message) -> R,
    invalid: Message,
    overflow: impl FnOnce(usize, usize) -> Message,
) -> Option<Vec<R>> {
    // Inspect all raw entries before the display cap, so a later conflict cannot leave a measured survivor.
    let positions = RecordedOperationalRead::replay_positions(originals);
    let count = positions.len();
    if count == 0 {
        return None;
    }
    let mut reports = positions
        .into_iter()
        .take(MAX_RECORDED_OPERATIONAL_READS)
        .map(|position| {
            position.and_then(|index| interpret(&originals[index])).unwrap_or_else(|_| unavailable(&invalid))
        })
        .collect::<Vec<_>>();
    if count > MAX_RECORDED_OPERATIONAL_READS {
        reports.push(unavailable(&overflow(MAX_RECORDED_OPERATIONAL_READS, count)));
    }
    Some(reports)
}

#[cfg(test)]
mod tests;
