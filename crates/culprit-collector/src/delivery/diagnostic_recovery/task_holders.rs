//! Cold verification reloads the exact paired original captures, never a current baseline or native reader.
use super::*;

pub(super) async fn authenticate_episode(
    store: &dyn RecoveryStore,
    contract: &crate::delivery::contract::Contract,
    episode: &DiagnosticEpisode,
) -> Result<()> {
    let DiagnosticSignal::PageTaskWait { parent, page, .. } = &episode.signal else { return Err(invalid()) };
    let (original, page_source, point) = originals(store, contract, parent, page).await?;
    if !matches!(&page_source.observation, SourceObservation::PageWaitTargets { original_activity: Some(activity), .. }
        if *activity == parent.capture_ref())
    {
        return Err(invalid());
    }
    let rows = input_rows(&original)?;
    let activity = activity_rows(&rows)?;
    let frame = ActivityFrameEvidence::new(&original.range().manifest().source, &activity, true)?;
    let DiagnosticFocusPlan::Ready(focus) = plan_page_task_holder_focus(&page_source, &point, page, &frame)? else {
        return Err(invalid());
    };
    if !episode.matches_focus(&focus) || episode.scope != DiagnosticScope::from_source(frame.source())? {
        return Err(invalid());
    }
    Ok(())
}

pub(super) async fn authenticate_focus(
    store: &dyn RecoveryStore,
    contract: &crate::delivery::contract::Contract,
    focus: &PageTaskHolderFocus,
) -> Result<()> {
    let (original, source, point) = originals(store, contract, &focus.participant.parent, &focus.page).await?;
    let rows = input_rows(&original)?;
    let activity = activity_rows(&rows)?;
    let frame = ActivityFrameEvidence::new(&original.range().manifest().source, &activity, true)?;
    if !focus.matches_activity_frame(&frame) || !focus.matches_page(&source, &point, &focus.page) {
        return Err(invalid());
    }
    Ok(())
}

async fn originals(
    store: &dyn RecoveryStore,
    contract: &crate::delivery::contract::Contract,
    parent: &DiagnosticEvidenceRef,
    page: &DiagnosticEvidenceRef,
) -> Result<(RecoveredDiagnosticInput, CollectionDescription, PageWaitObservation)> {
    let original = load_diagnostic_input(store, &range_id_for(contract, &parent.capture)?).await?;
    verify_reference(original.range().manifest(), parent)?;
    if !matches!(original.range().manifest().source.observation, SourceObservation::Activity(_)) {
        return Err(invalid());
    }
    let recovered = load_diagnostic_evidence(store, page).await?;
    let SourceObservation::PageWaitTargets { original_activity, .. } = &recovered.source().observation else {
        return Err(invalid());
    };
    if original_activity.as_ref().is_some_and(|activity| *activity != parent.capture_ref()) {
        return Err(invalid());
    }
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(point), .. }) =
        &recovered.record().record
    else {
        return Err(invalid());
    };
    Ok((original, recovered.source().clone(), (**point).clone()))
}
