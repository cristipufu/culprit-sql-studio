//! Native driver outcomes; bounded accounting is shared with other providers.
use crate::read::{ReadError, ReadOutcome};
use culprit_core::telemetry::{CollectionStop, ReadTaskOutcome};
pub(crate) use culprit_core::telemetry::{ReadMetricsRecorder as Metrics, ReadProgress as Progress};
pub(crate) fn requested(outcome: &ReadOutcome) -> ReadTaskOutcome {
    match outcome {
        ReadOutcome::QuiescentStop(reason) | ReadOutcome::NeedsAttention(ReadError::Stopped(reason)) => {
            ReadTaskOutcome::stopped(*reason)
        }
        ReadOutcome::NeedsAttention(ReadError::Source(error)) => {
            let reason = CollectionStop::from_error(error);
            ReadTaskOutcome {
                timed_out: reason == CollectionStop::TimedOut,
                cancelled: reason == CollectionStop::Cancelled,
                ..Default::default()
            }
        }
        ReadOutcome::Completed(_) => ReadTaskOutcome::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::{Engine, Error, ErrorCode, telemetry::ReadWorkMetrics};
    use std::time::Duration;

    fn recorded(outcome: ReadOutcome, unconfirmed: bool) -> ReadWorkMetrics {
        let mut accounting = requested(&outcome);
        accounting.failed = !matches!(outcome, ReadOutcome::Completed(_));
        accounting.unconfirmed = unconfirmed;
        let recorder = Metrics::new(Engine::Mssql);
        let id = recorder.begin("SELECT N'private-native-stop-canary'");
        recorder.finish(id.as_deref(), Duration::ZERO, &Progress::default(), &accounting);
        recorder.snapshot().unwrap()
    }

    #[test]
    fn explicit_owner_requests_remain_exact_after_final_cancellation_is_unconfirmed() {
        for reason in
            [CollectionStop::ByteLimit, CollectionStop::RowLimit, CollectionStop::TimedOut, CollectionStop::Cancelled]
        {
            let metrics = recorded(ReadOutcome::NeedsAttention(ReadError::Stopped(reason)), true);
            assert!(metrics.is_valid());
            assert!(metrics.has_unconfirmed_work());
            let metric = &metrics.templates[0];
            assert_eq!(metric.stop_causes, [culprit_core::telemetry::ReadStopCount { reason, tasks: 1 }]);
            assert_eq!(metric.unconfirmed_cancellations, 1);
            assert_eq!(metric.timeouts, u32::from(reason == CollectionStop::TimedOut));
            assert_eq!(metric.cancellations, u32::from(reason == CollectionStop::Cancelled));
        }
    }

    #[test]
    fn completed_source_errors_and_lost_protocol_proof_never_invent_an_owner_request() {
        for code in
            [ErrorCode::PermissionDenied, ErrorCode::ConnectionFailed, ErrorCode::QueryFailed, ErrorCode::Timeout]
        {
            for unconfirmed in [false, true] {
                let metrics = recorded(
                    ReadOutcome::NeedsAttention(ReadError::Source(Error::new(code, "private-source-error-canary"))),
                    unconfirmed,
                );
                assert!(metrics.is_valid());
                let metric = &metrics.templates[0];
                assert!(metric.stop_causes.is_empty());
                assert_eq!(metric.failures, 1);
                assert_eq!(metric.timeouts, u32::from(code == ErrorCode::Timeout));
                assert_eq!(metric.unconfirmed_cancellations, u32::from(unconfirmed));
                assert!(
                    !serde_json::to_string(&metrics.summary(None).unwrap())
                        .unwrap()
                        .contains("private-source-error-canary")
                );
            }
        }
        let metrics = recorded(ReadOutcome::NeedsAttention(ReadError::Source(crate::read::invalid_data())), true);
        assert!(metrics.templates[0].stop_causes.is_empty(), "typed source validation errors are not owner requests");
    }

    #[test]
    fn normal_completion_and_unknown_unconfirmed_trigger_have_no_fabricated_cause() {
        let completed = recorded(ReadOutcome::Completed(vec![]), false);
        assert!(completed.is_valid());
        assert!(completed.templates[0].stop_causes.is_empty());
        assert_eq!(completed.templates[0].failures, 0);
        let unknown =
            recorded(ReadOutcome::NeedsAttention(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)), true);
        assert!(unknown.is_valid());
        assert!(unknown.templates[0].stop_causes.is_empty());
        assert!(unknown.has_unconfirmed_work());
    }
}
