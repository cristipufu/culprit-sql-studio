//! Presentation uses the persisted answer; it never rebinds a query or infers a failed execution's cause.
use super::*;

pub(super) fn append(answer: &QueryCompilationAnswer, reasons: &mut Vec<String>) {
    reasons.push(match &answer.selection {
        QueryCompilationSelection::Observed { counters, .. } => t!(
            "view.incident_history.compilation.observed",
            count = format::digits(counters.compilations.clone()),
            duration = t!("common.duration.us", n = format::digits(counters.average_duration_us.to_string())),
            last = &counters.last_compile_at,
        ),
        QueryCompilationSelection::NotObserved => t!("view.incident_history.compilation.not_observed"),
        QueryCompilationSelection::IncarnationChanged => t!("view.incident_history.compilation.changed"),
    });
    reasons.push(t!("view.incident_history.compilation.independent"));
    reasons.push(t!(
        "view.incident_history.compilation.read_bounds",
        from = &answer.result_context.observed_from,
        until = &answer.result_context.observed_until,
        original_from = &answer.original_interval.from,
        original_until = &answer.original_interval.until
    ));
    match answer.source_state {
        SourceState::Partial => reasons.push(t!("view.compilation.limit.source_partial")),
        SourceState::Unavailable | SourceState::NotApplicable => {
            reasons.push(t!("view.compilation.limit.source_unavailable"))
        }
        SourceState::Collected | SourceState::Empty => (),
    }
    reasons.extend(answer.unresolved.iter().map(|gap| match gap {
        QueryCompilationGap::ActualExecutionPlan => t!("view.incident_history.compilation.gap.actual_plan"),
        QueryCompilationGap::RuntimeInputs => t!("view.incident_history.compilation.gap.inputs"),
        QueryCompilationGap::ComparableExecutionWork => t!("view.incident_history.compilation.gap.work"),
        QueryCompilationGap::BlockingAndGrantDemand => t!("view.incident_history.compilation.gap.waits"),
        QueryCompilationGap::StatisticsAndForcingContext => t!("view.incident_history.compilation.gap.context"),
    }));
}
