use super::*;
use crate::local_captures::blocking::LocalActivityRow;
use culprit_collector::outbox::{CompletedActivity, CompletedBlocking};

pub(super) fn explain(capture: CompletedBlocking) -> Result<LocalBlockingAnalysis> {
    let report = {
        let frame = capture.followup().frame()?;
        let rows = frame.rows().collect::<Vec<_>>();
        Box::new(culprit_checks::focused_blocking::explain(frame.source(), &rows, true)?)
    };
    let bytes_read = capture.bytes_read();
    let (original, followup) = capture.into_inputs();
    Ok(LocalBlockingAnalysis { original: local_read(original)?, followup: local_read(followup)?, report, bytes_read })
}
pub(super) fn local_read(capture: CompletedActivity) -> Result<LocalActivityRead> {
    let mut read = LocalActivityRead {
        connection: capture.connection().into(),
        source: capture.source().clone(),
        capture: capture.capture().clone(),
        rows: vec![],
        collected_from: capture.collected_from().into(),
        collected_until: capture.collected_until().into(),
        bytes_read: capture.bytes_read(),
    };
    read.rows = capture
        .into_rows()
        .into_iter()
        .map(|row| {
            let TelemetryRecord::Activity(observation) = row.row.record else {
                return Err(invalid());
            };
            Ok(LocalActivityRow { observation, reference: row.reference })
        })
        .collect::<Result<_>>()?;
    read.frame()?;
    Ok(read)
}
