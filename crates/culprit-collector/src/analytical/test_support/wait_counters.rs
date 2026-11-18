//! One controlled wait corpus exercises original capture/projection and the same scan/archive explanation.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitCounterCase {
    Exact,
    InconsistentSignal,
    NativePartial,
    Empty,
    Denied,
}
impl WaitCounterCase {
    pub const ALL: [Self; 5] = [Self::Exact, Self::InconsistentSignal, Self::NativePartial, Self::Empty, Self::Denied];
}

pub fn wait_counter_evidence(case: WaitCounterCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut rows) = rbio_wait_evidence();
    let TelemetryRecord::WaitCounter(template) = &rows[0] else { panic!() };
    let template = template.clone();
    for (name, total, signal) in [("SLEEP_TASK", "2", "0"), ("WRITELOG", "3", "1")] {
        rows.push(TelemetryRecord::WaitCounter(WaitCounterObservation {
            wait_type: name.into(),
            mechanism: None,
            wait_time_ms: total.into(),
            signal_wait_time_ms: signal.into(),
            ..template.clone()
        }));
    }
    if case == WaitCounterCase::InconsistentSignal {
        let TelemetryRecord::WaitCounter(row) = &mut rows[0] else { panic!() };
        row.wait_time_ms = "1".into();
        row.signal_wait_time_ms = "2".into();
    }
    if matches!(case, WaitCounterCase::Empty | WaitCounterCase::Denied) {
        rows.clear();
    }
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!() };
    snapshot.observed_from = "2020-01-01T12:00:00.000000001Z".into();
    snapshot.observed_until = "2020-01-01T12:00:00.000000009Z".into();
    snapshot.records = rows.len() as u32;
    snapshot.sources = vec![match case {
        WaitCounterCase::Denied => StateSourceCoverage::unavailable(
            culprit_core::source_id::WAITS.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        ),
        _ => StateSourceCoverage::observed(
            culprit_core::source_id::WAITS.into(),
            rows.len() as u32,
            case != WaitCounterCase::NativePartial,
        ),
    }];
    (source, rows)
}
