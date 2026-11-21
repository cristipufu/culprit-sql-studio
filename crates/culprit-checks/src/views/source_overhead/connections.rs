//! Source acquisition observations describe this capture route, never application connectivity or native cause.
use culprit_core::{SourceConnectionExplanation, format, t, telemetry::*};
pub(super) fn explain(work: &ReadConnectionWork) -> SourceConnectionExplanation {
    let mut limitations = vec![
        t!("view.source_overhead.connection.limit.scope"),
        t!("view.source_overhead.connection.limit.clock"),
        t!("view.source_overhead.connection.limit.phase"),
    ];
    if !work.has_complete_observation_roster() {
        limitations.push(t!(
            "view.source_overhead.connection.limit.partial",
            attempts = format::digits(work.omitted_attempts.to_string()),
            phases = format::digits(work.omitted_phases.to_string())
        ));
    }
    if work.has_unconfirmed_work() {
        limitations.push(t!("view.source_overhead.connection.limit.unconfirmed"));
    }
    if work.attempts.iter().flat_map(|attempt| &attempt.phases).any(|phase| !phase.has_ordered_client_clocks()) {
        limitations.push(t!("view.source_overhead.connection.limit.clock_changed"));
    }
    let observations = work
        .attempts
        .iter()
        .flat_map(|attempt| {
            let mut entries = vec![t!(
                "view.source_overhead.connection.attempt",
                ordinal = format::digits(attempt.ordinal.to_string()),
                usage = usage(attempt.connection_use)
            )];
            entries.extend(attempt.phases.iter().map(|phase| {
                t!(
                    "view.source_overhead.connection.phase",
                    stage = stage(phase.stage),
                    outcome = outcome(&phase.outcome),
                    elapsed = phase
                        .client_elapsed_ms
                        .map(format::duration)
                        .unwrap_or_else(|| t!("view.source_overhead.connection.unknown")),
                    from = &phase.client_started_at,
                    until = phase
                        .client_finished_at
                        .clone()
                        .unwrap_or_else(|| t!("view.source_overhead.connection.unknown"))
                )
            }));
            entries
        })
        .collect();
    SourceConnectionExplanation {
        summary: t!(
            "view.source_overhead.connection.summary",
            attempts = format::digits(work.attempted.to_string()),
            shown = format::digits(work.attempts.len().to_string())
        ),
        observations,
        limitations,
    }
}
fn usage(value: ReadConnectionUse) -> String {
    match value {
        ReadConnectionUse::Unknown => t!("view.source_overhead.connection.use.unknown"),
        ReadConnectionUse::FreshConnection => t!("view.source_overhead.connection.use.fresh"),
        ReadConnectionUse::AuthenticatedReuse => t!("view.source_overhead.connection.use.reused"),
    }
}
fn stage(value: ReadConnectionStage) -> String {
    match value {
        ReadConnectionStage::Acquire => t!("view.source_overhead.connection.stage.acquire"),
        ReadConnectionStage::RouteSelection => t!("view.source_overhead.connection.stage.endpoint"),
        ReadConnectionStage::Transport => t!("view.source_overhead.connection.stage.transport"),
        ReadConnectionStage::HandshakeAndLogin => t!("view.source_overhead.connection.stage.login"),
        ReadConnectionStage::SessionSetup => t!("view.source_overhead.connection.stage.setup"),
        ReadConnectionStage::FirstQuery => t!("view.source_overhead.connection.stage.query"),
    }
}
fn outcome(value: &ReadConnectionOutcome) -> String {
    match value {
        ReadConnectionOutcome::Pending => t!("view.source_overhead.connection.outcome.pending"),
        ReadConnectionOutcome::Completed => t!("view.source_overhead.connection.outcome.completed"),
        ReadConnectionOutcome::Redirected => t!("view.source_overhead.connection.outcome.redirected"),
        ReadConnectionOutcome::Failed { code, native_code: Some(native) } => t!(
            "view.source_overhead.connection.outcome.native_error",
            code = culprit_core::wire_name(code),
            native = format::digits(native.to_string())
        ),
        ReadConnectionOutcome::Failed { code, native_code: None } => {
            t!("view.source_overhead.connection.outcome.error", code = culprit_core::wire_name(code))
        }
        ReadConnectionOutcome::Stopped { reason } => {
            t!("view.source_overhead.connection.outcome.stopped", reason = culprit_core::wire_name(reason))
        }
        ReadConnectionOutcome::Unconfirmed => t!("view.source_overhead.connection.outcome.unconfirmed"),
    }
}
