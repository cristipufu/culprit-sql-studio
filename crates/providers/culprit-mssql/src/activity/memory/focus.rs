//! Select the original native grant participant before any guarded SQL enrichment; absence is not recovery.
use super::*;

pub(crate) struct FocusedMemoryCapture {
    pub capture: MemoryCapture,
    pub result: ActivityRequestMemoryFocusResult,
}

impl<R: NativeRead> ActivityReader<R> {
    pub(crate) async fn capture_focused_memory(
        &mut self,
        focus: &ActivityRequestFocus,
    ) -> Result<FocusedMemoryCapture> {
        if !focus.is_request_memory_focus()
            || focus.source != culprit_core::source_id::SESSIONS
            || super::super::RequestLocator::from_identity(&focus.statement_identity).is_none()
        {
            return Err(invalid_data());
        }
        let mut frame = self.observe_memory_for(Some(focus)).await?;
        let membership = select(&mut frame, focus)?;
        self.enrich_memory(&mut frame).await?;
        let result = ActivityRequestMemoryFocusResult {
            focus: focus.clone(),
            observed_from: frame.owner.activity.at.clone(),
            observed_until: frame.context.at.clone(),
            source: FocusSourceState::Matched,
            membership,
        };
        let capture = MemoryCapture {
            owner: frame.owner.clone(),
            context: frame.context.clone(),
            complete_scope: frame.complete_scope,
            observations: frame.into_observations()?,
        };
        if !result.is_valid() || capture.observations.iter().any(|row| !result.accepts_observation(row)) {
            return Err(invalid_data());
        }
        Ok(FocusedMemoryCapture { capture, result })
    }
}

pub(super) fn matches_owner(focus: &ActivityRequestFocus, owner: &crate::local_source::Context) -> Result<()> {
    if focus.database_writable.is_none() || !owner.is_qualified() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let context = owner.capture_context()?;
    if focus.target.database() != Some(&context.database)
        || focus.partition != owner.source_partition()?
        || context.epoch.as_deref() != Some(focus.epoch.as_str())
        || owner.activity.writable != focus.database_writable
        || ObservationBounds::new(&focus.original_observed_at, &owner.activity.at).is_none()
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(())
}

fn select(frame: &mut MemoryFrame, focus: &ActivityRequestFocus) -> Result<RequestMemoryMembership> {
    let original = &focus.original;
    let numeric_key =
        format!("{}:{}", original.key.session.id, original.key.request.as_ref().ok_or_else(invalid_data)?.id);
    let unverified = frame.grants.iter().any(|row| {
        row.identity.id == numeric_key
            && row.request.as_ref().is_none_or(|request| {
                request.key != original.key || request.statement_identity.as_ref() != Some(&focus.statement_identity)
            })
    });
    frame.activity.records.retain(|row| {
        row.facts.key == original.key && row.facts.statement_identity.as_ref() == Some(&focus.statement_identity)
    });
    frame.grants.retain(|row| {
        row.request.as_ref().is_some_and(|request| {
            request.key == original.key && request.statement_identity.as_ref() == Some(&focus.statement_identity)
        })
    });
    if frame.grants.len() > 1 || frame.activity.records.len() > 1 {
        return Err(invalid_data());
    }
    Ok(if !frame.grants.is_empty() {
        RequestMemoryMembership::Observed
    } else if unverified {
        RequestMemoryMembership::Unverified
    } else {
        RequestMemoryMembership::NotObserved
    })
}

#[cfg(test)]
pub(crate) mod tests;
