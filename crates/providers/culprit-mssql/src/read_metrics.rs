//! Native driver outcomes; bounded accounting is shared with other providers.
use crate::read::{ReadError, ReadOutcome};
use culprit_core::telemetry::{CollectionStop, ReadTaskOutcome};
pub(crate) use culprit_core::telemetry::{ReadMetricsRecorder as Metrics, ReadProgress as Progress};
pub(crate) fn requested(outcome: &ReadOutcome) -> ReadTaskOutcome {
    let reason = match outcome {
        ReadOutcome::QuiescentStop(reason) | ReadOutcome::NeedsAttention(ReadError::Stopped(reason)) => Some(*reason),
        ReadOutcome::NeedsAttention(ReadError::Source(error)) => Some(CollectionStop::from_error(error)),
        _ => None,
    };
    ReadTaskOutcome {
        timed_out: reason == Some(CollectionStop::TimedOut),
        cancelled: reason == Some(CollectionStop::Cancelled),
        ..Default::default()
    }
}
