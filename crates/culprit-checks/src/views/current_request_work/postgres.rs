//! Native COPY counts remain command-local evidence, without a phase, elapsed-work estimate or completion claim.
use super::*;

pub(super) fn limitations() -> [String; 3] {
    [
        t!("view.current_request_work.copy.limit.total"),
        t!("view.current_request_work.copy.limit.counters"),
        t!("view.current_request_work.copy.limit.scope"),
    ]
}

pub(super) fn details(progress: &PostgresCopyProgress) -> Details {
    let mut metrics = vec![
        Metric::said_text(key!("view.current_request_work.copy.metric.command"), progress.command.as_native()),
        Metric::said_text(key!("view.current_request_work.copy.metric.io"), progress.io_type.as_native()),
        Metric::said_text(
            key!("view.current_request_work.copy.metric.database_oid"),
            format::digits(progress.database_oid.clone()),
        ),
        Metric::said_text(
            key!("view.current_request_work.copy.metric.relation_oid"),
            format::digits(progress.relation_oid.clone()),
        ),
    ];
    metrics.extend(counter_metrics([
        (key!("view.current_request_work.copy.metric.bytes_processed"), &progress.bytes_processed),
        (key!("view.current_request_work.copy.metric.bytes_total"), &progress.bytes_total),
        (key!("view.current_request_work.copy.metric.tuples_processed"), &progress.tuples_processed),
        (key!("view.current_request_work.copy.metric.tuples_excluded"), &progress.tuples_excluded),
        (key!("view.current_request_work.copy.metric.tuples_skipped"), &progress.tuples_skipped),
    ]));
    let explanation = if progress.relation_oid == "0" {
        t!("view.current_request_work.copy.entry.query")
    } else {
        t!("view.current_request_work.copy.entry.table")
    };
    Details {
        explanation,
        metrics,
        investigation: t!("view.current_request_work.copy.investigate"),
        verify: t!("view.current_request_work.copy.verify"),
    }
}
