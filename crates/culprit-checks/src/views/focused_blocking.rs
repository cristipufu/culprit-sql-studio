//! Presentation of the existing native Activity/graph evidence, without another blocking classifier.
use culprit_core::{BlockingReport, Error, Result, SourceState, format, t, telemetry::*};

pub fn explain(
    source: &CollectionDescription,
    rows: &[ActivityFrameRow<'_>],
    read_complete: bool,
) -> Result<BlockingReport> {
    let SourceObservation::FocusedBlocking { snapshot, result } = &source.observation else { return Err(invalid()) };
    let frame = ActivityFrameEvidence::new(source, rows, read_complete)?;
    if !result.accepts_frame(&frame) {
        return Err(invalid());
    }
    let observations = rows.iter().map(|row| row.observation.clone()).collect::<Vec<_>>();
    let relationships = ActivityEvidence::blocking_observations(&observations)?;
    let blocking = culprit_core::blocking::analyze_activity(&frame)?.map(Box::new);
    let maintenance = super::maintenance::explain(source, rows, read_complete)?;
    let mut limitations =
        vec![t!("view.blocking.limit.point"), t!("view.blocking.limit.session"), t!("view.blocking.limit.population")];
    if result.binding == FocusedRequestBinding::UnverifiedOriginalScope {
        limitations.push(t!("view.blocking.limit.original_scope"));
    }
    if !frame.context().has_qualified_local_owner() {
        limitations.push(t!("view.blocking.limit.current_scope"));
    }
    if !read_complete {
        limitations.push(t!("view.blocking.limit.read"));
    }
    if snapshot.coverage.error_code == Some(culprit_core::ErrorCode::PermissionDenied) {
        limitations.push(t!("view.blocking.limit.visibility"));
    }
    if result.closure.is_limited() {
        limitations.push(t!(
            "view.blocking.limit.bounds",
            lookups = format::digits(result.closure.max_lookups.to_string()),
            depth = format::digits(result.closure.max_depth.to_string()),
            participants = format::digits(result.closure.max_participants.to_string())
        ));
    }
    if relationships.iter().any(|edge| !edge.owner_observed) {
        limitations.push(t!("view.blocking.limit.owner_context"));
    }
    if relationships.iter().any(BlockingObservation::reports_waiter_session_id) {
        limitations.push(t!("view.blocking.limit.self_session"));
    }
    if relationships.iter().any(|edge| matches!(&edge.owner, ActivityWaitOwner::Transaction { owner: TransactionWaitOwner::Prepared, native_id } if native_id == "0")) {
        limitations.push(t!("view.blocking.limit.prepared"));
    }
    if matches!(result.request_end, BlockingRequestState::Changed | BlockingRequestState::Missing) {
        limitations.push(t!("view.blocking.limit.request_end"));
    }
    Ok(BlockingReport {
        source: source.source.clone(),
        context: frame.context(),
        source_state: if !read_complete
            && matches!(snapshot.coverage.state, SourceState::Collected | SourceState::Empty)
        {
            SourceState::Partial
        } else {
            snapshot.coverage.state
        },
        source_reason: snapshot.coverage.stopped.map(|stop| stop.error().message),
        read_complete,
        native_rows: snapshot.native_rows,
        observed_rows: rows.len() as u32,
        original: result.focus.parent.clone(),
        binding: result.binding,
        membership: result.membership,
        request_end: result.request_end,
        closure: result.closure.clone(),
        summary: summary(result, rows.len(), snapshot.observations, relationships.len()),
        relationships,
        blocking,
        maintenance: (maintenance.maintenance_observations > 0).then(|| Box::new(maintenance)),
        title: t!("view.blocking.title"),
        limitations,
        investigation: t!("view.blocking.investigation"),
        verify: t!("view.blocking.verify"),
    })
}

fn summary(result: &ActivityBlockingFocusResult, observed: usize, native: u32, edges: usize) -> String {
    match result.binding {
        FocusedRequestBinding::ParticipantMissing => t!("view.blocking.summary.missing"),
        FocusedRequestBinding::ParticipantChanged => t!("view.blocking.summary.changed"),
        _ if observed != native as usize => t!("view.blocking.summary.read_gap"),
        _ => match result.membership {
            FocusedRequestMembership::Unavailable => t!("view.blocking.summary.unavailable"),
            FocusedRequestMembership::NotObserved => t!("view.blocking.summary.not_observed"),
            FocusedRequestMembership::Observed if result.closure.status == BlockingClosureStatus::NotRead => {
                t!("view.blocking.summary.not_read")
            }
            FocusedRequestMembership::Observed if edges == 0 => t!("view.blocking.summary.no_relationships"),
            FocusedRequestMembership::Observed => {
                t!("view.blocking.summary.observed", n = edges, count = format::digits(edges.to_string()))
            }
        },
    }
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
