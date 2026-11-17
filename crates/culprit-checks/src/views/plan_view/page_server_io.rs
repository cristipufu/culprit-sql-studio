//! The same small native-facts projection serves interactive and authenticated archived plans.
use culprit_core::actual::page_server_io::{PlanPageServerIo, PlanPageServerMetric};
use culprit_core::{Metric, PlanNode, format, key, plan_view::PlanPageServerIoView, t};

pub(super) fn explain(node: &PlanNode) -> Option<Box<PlanPageServerIoView>> {
    let io = node.runtime.as_ref()?.page_server_io.as_ref()?;
    if !io.is_valid() {
        return Some(Box::new(PlanPageServerIoView {
            metrics: vec![],
            limitations: vec![t!("view.plan_page_server_io.invalid")],
        }));
    }
    let mut limitations = vec![t!("view.plan_page_server_io.limit.recorded")];
    if !io.issues.is_empty() {
        limitations.push(t!("view.plan_page_server_io.limit.partial"));
    }
    Some(Box::new(PlanPageServerIoView {
        metrics: vec![
            metric(io, PlanPageServerMetric::Reads, key!("view.plan_page_server_io.reads")),
            metric(io, PlanPageServerMetric::ReadAheads, key!("view.plan_page_server_io.read_aheads")),
            metric(io, PlanPageServerMetric::LobReads, key!("view.plan_page_server_io.lob_reads")),
            metric(io, PlanPageServerMetric::LobReadAheads, key!("view.plan_page_server_io.lob_read_aheads")),
        ],
        limitations,
    }))
}

fn metric(io: &PlanPageServerIo, metric: PlanPageServerMetric, key: &str) -> Metric {
    Metric::said_text(
        key,
        io.value(metric).map_or_else(|| t!("view.plan_page_server_io.unknown"), |value| format::digits(value.into())),
    )
}

#[cfg(test)]
mod tests;
