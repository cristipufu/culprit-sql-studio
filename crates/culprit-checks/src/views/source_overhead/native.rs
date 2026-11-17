//! Recorded native profile semantics, independent of the engine selected for a view.
use culprit_core::{SourceNativeIntervalExplanation, format, t, telemetry::*};

pub(super) fn explain(observation: &NativeSessionReadSummary) -> SourceNativeIntervalExplanation {
    match observation {
        NativeSessionReadSummary::MssqlSession { reference, settlement, issue, binding, state, interval } => {
            let summary = match state {
                NativeReadWorkState::Observing => t!("view.source_overhead.native.summary.observing"),
                NativeReadWorkState::Settled => t!("view.source_overhead.native.summary.settled"),
                NativeReadWorkState::Unavailable => t!("view.source_overhead.native.summary.unavailable"),
            };
            let mut limitations = vec![
                t!("view.source_overhead.native.limit.interval"),
                t!("view.source_overhead.native.limit.io"),
                t!("view.source_overhead.native.limit.memory"),
            ];
            if let Some(issue) = issue {
                limitations.push(issue_text(*issue));
            }
            match settlement {
                NativeReadSettlement::Pending => limitations.push(t!("view.source_overhead.native.limit.pending")),
                NativeReadSettlement::Unconfirmed => {
                    limitations.push(t!("view.source_overhead.native.limit.unconfirmed"))
                }
                NativeReadSettlement::Quiescent => {}
            }
            match binding {
                NativeReadOwnerBinding::Unverified => limitations.push(t!("view.source_overhead.native.limit.mapping")),
                NativeReadOwnerBinding::Contradictory => {
                    limitations.push(t!("view.source_overhead.native.limit.contradictory"))
                }
                NativeReadOwnerBinding::Matched => {}
            }
            let mut measurements = vec![t!("view.source_overhead.native.reference", key = reference)];
            if let Some(interval) = interval {
                if let Some(until) = &interval.observed_until {
                    measurements.push(t!(
                        "view.source_overhead.native.bounds",
                        from = &interval.observed_from,
                        until = until,
                        first = format::digits(interval.first_query_ordinal.to_string()),
                        last = interval
                            .last_query_ordinal
                            .map(|value| format::digits(value.to_string()))
                            .unwrap_or_else(unknown)
                    ));
                }
                if *state == NativeReadWorkState::Settled {
                    measurements.push(t!(
                        "view.source_overhead.native.counters",
                        cpu = value(&interval.cpu_ms),
                        reads = value(&interval.reads),
                        logical = value(&interval.logical_reads),
                        dirty = value(&interval.dirty_marks)
                    ));
                    measurements.push(t!(
                        "view.source_overhead.native.memory",
                        before = value(&interval.memory_pages_initial),
                        after = value(&interval.memory_pages_latest)
                    ));
                    limitations.push(t!("view.source_overhead.native.limit.resolution"));
                }
            }
            SourceNativeIntervalExplanation { summary, measurements, limitations }
        }
    }
}
fn value(value: &Option<String>) -> String {
    value.as_ref().map(|value| format::digits(value.clone())).unwrap_or_else(unknown)
}
fn unknown() -> String {
    t!("view.source_overhead.native.unknown")
}
fn issue_text(issue: NativeReadWorkIssue) -> String {
    match issue {
        NativeReadWorkIssue::MissingSession => t!("view.source_overhead.native.issue.session"),
        NativeReadWorkIssue::MissingCounters => t!("view.source_overhead.native.issue.counters"),
        NativeReadWorkIssue::NativeScopeUnqualified => t!("view.source_overhead.native.issue.scope"),
        NativeReadWorkIssue::InvalidObservation => t!("view.source_overhead.native.issue.invalid"),
        NativeReadWorkIssue::SourceChanged => t!("view.source_overhead.native.issue.source_changed"),
        NativeReadWorkIssue::SessionChanged => t!("view.source_overhead.native.issue.session_changed"),
        NativeReadWorkIssue::ConnectionChanged => t!("view.source_overhead.native.issue.connection_changed"),
        NativeReadWorkIssue::CounterDecreased => t!("view.source_overhead.native.issue.decreased"),
        NativeReadWorkIssue::ClockChanged => t!("view.source_overhead.native.issue.clock"),
        NativeReadWorkIssue::MissingFinalObservation => t!("view.source_overhead.native.issue.final"),
    }
}
