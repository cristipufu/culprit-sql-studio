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
        let selected = select(&mut frame, focus)?;
        let original =
            if selected.subject.is_some() { BTreeSet::from([focus.original.key.clone()]) } else { BTreeSet::new() };
        self.enrich_memory_for(&mut frame, &original).await?;
        let result = ActivityRequestMemoryFocusResult {
            focus: focus.clone(),
            observed_from: frame.owner.activity.at.clone(),
            observed_until: frame.context.at.clone(),
            source: FocusSourceState::Matched,
            membership: selected.membership,
            subject: selected.subject,
            resource: selected.resource,
            peers: selected.peers,
        };
        let capture = MemoryCapture {
            owner: frame.owner.clone(),
            context: frame.context.clone(),
            complete_scope: frame.complete_scope,
            observations: frame.into_observations()?,
        };
        if !result.matches_observations(&capture.observations) {
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

struct Selection {
    membership: RequestMemoryMembership,
    subject: Option<NativeIdentity>,
    resource: Option<NativeIdentity>,
    peers: Vec<NativeIdentity>,
}

fn select(frame: &mut MemoryFrame, focus: &ActivityRequestFocus) -> Result<Selection> {
    let original = &focus.original;
    let numeric_key =
        format!("{}:{}", original.key.session.id, original.key.request.as_ref().ok_or_else(invalid_data)?.id);
    let unverified = frame.grants.iter().any(|row| {
        row.identity.id == numeric_key
            && row.request.as_ref().is_none_or(|request| {
                request.key != original.key || request.statement_identity.as_ref() != Some(&focus.statement_identity)
            })
    });
    let database = focus.target.database().ok_or_else(invalid_data)?;
    let mut subjects = frame.grants.iter().filter(|row| {
        row.is_admission_participant(&database.database)
            && row.request.as_ref().is_some_and(|request| {
                request.key == original.key && request.statement_identity.as_ref() == Some(&focus.statement_identity)
            })
    });
    let subject = subjects.next().cloned();
    if subjects.next().is_some() {
        return Err(invalid_data());
    }
    let peers = subject
        .as_ref()
        .map(|subject| {
            frame
                .grants
                .iter()
                .filter(|row| subject.shares_admission_resource(row, &database.database))
                .map(|row| row.identity.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    frame.grants.retain(|row| {
        subject.as_ref().is_some_and(|subject| row.identity == subject.identity) || peers.contains(&row.identity)
    });
    frame.activity.records.retain(|row| {
        frame.grants.iter().any(|grant| {
            grant.request.as_ref().is_some_and(|request| {
                request.key == row.facts.key && request.statement_identity == row.facts.statement_identity
            })
        })
    });
    Ok(Selection {
        membership: if subject.is_some() {
            RequestMemoryMembership::Observed
        } else if unverified {
            RequestMemoryMembership::Unverified
        } else {
            RequestMemoryMembership::NotObserved
        },
        resource: subject.as_ref().and_then(|row| row.resource.clone()),
        subject: subject.map(|row| row.identity),
        peers,
    })
}

#[cfg(test)]
pub(crate) mod tests;
