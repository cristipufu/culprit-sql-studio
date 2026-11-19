//! Historical participation is bound to the original Activity receipt, not a later request with the same ID.
use super::PageWaitActivityEvidence;
use crate::{Result, SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use ts_rs::TS;

/// Leave room in a 256-input diagnostic revision for original/peak/latest, result, admission and owner receipts.
/// All matching contexts are withheld if they do not fit; a MARS request is never chosen as representative.
pub const MAX_PAGE_WAIT_BLOCKING_CONTEXTS: usize = 64;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageWaitBlockingIssue {
    FrameUnavailable,
    NativeScopeUnqualified,
    ParallelTaskUnresolved,
    WaitOwnerUnavailable,
    OwnerGenerationUnavailable,
    OwnerContextUnavailable,
    SelfReference,
    MultipleOwnerContexts,
    ContextsLimited,
    ReadIncomplete,
    NativePopulationPartial,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PageWaitActivityReference {
    pub activity: ActivityKey,
    pub statement: Option<NativeIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub evidence: Option<DiagnosticEvidenceRef>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PageWaitBlockingBasis {
    pub source: String,
    pub context: NativeReadContext,
    pub native_epoch: Option<String>,
    pub observed_at: String,
    pub source_state: SourceState,
    pub read_complete: bool,
    pub waiter: PageWaitActivityReference,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PageWaitBlockingContext {
    pub reference: PageWaitActivityReference,
    pub scope: ActivityScopeBasis,
    pub state: String,
    pub command: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PageWaitBlockingParticipation {
    pub basis: Option<PageWaitBlockingBasis>,
    /// The original engine verdict, including untracked owners and missing generations; no pointer interpretation.
    pub reported_owners: Vec<ActivityWaitOwner>,
    pub contexts: Vec<PageWaitBlockingContext>,
    /// Number of positively matched captured contexts, including those withheld by the all-or-none bound.
    pub context_count: u32,
    pub issues: Vec<PageWaitBlockingIssue>,
}

impl PageWaitBlockingParticipation {
    /// Earlier coordinator blockers do not identify owners of a later native task wait.
    pub fn task_unresolved() -> Self {
        Self { issues: vec![PageWaitBlockingIssue::ParallelTaskUnresolved], ..Self::unavailable() }
    }

    pub fn unavailable() -> Self {
        Self {
            basis: None,
            reported_owners: vec![],
            contexts: vec![],
            context_count: 0,
            issues: vec![PageWaitBlockingIssue::FrameUnavailable],
        }
    }

    /// Secondary immutable rows only; the original waiter already has its own admitted parent receipt.
    pub fn references(&self) -> impl Iterator<Item = &DiagnosticEvidenceRef> {
        self.contexts.iter().filter_map(|context| context.reference.evidence.as_ref())
    }
}

pub fn page_wait_blocking_participation(
    source: &CollectionDescription,
    page: &PageWaitObservation,
    original: &ActivityRequestFocus,
    frame: &ActivityFrameEvidence<'_>,
) -> Result<PageWaitBlockingParticipation> {
    validate_source_binding(source, page, original, frame)?;
    let Some(parent) = frame.row(&original.original.key) else {
        if frame.read_complete() {
            return Err(CollectionStop::InvalidPage.error());
        }
        let mut unavailable = PageWaitBlockingParticipation::unavailable();
        unavailable.issues.push(PageWaitBlockingIssue::ReadIncomplete);
        return Ok(unavailable);
    };
    let native = frame.native();
    let mut participation = PageWaitBlockingParticipation {
        basis: Some(PageWaitBlockingBasis {
            source: frame.source().source.clone(),
            context: frame.context(),
            native_epoch: native.epoch.clone(),
            observed_at: native.at.clone(),
            source_state: native.coverage.state,
            read_complete: frame.read_complete(),
            waiter: reference(parent),
        }),
        reported_owners: vec![],
        contexts: vec![],
        context_count: 0,
        issues: vec![],
    };
    if !frame.read_complete() {
        participation.issues.push(PageWaitBlockingIssue::ReadIncomplete);
    }
    if !native.coverage.complete_scope {
        participation.issues.push(PageWaitBlockingIssue::NativePopulationPartial);
    }
    if page.task_wait.is_some() {
        participation.issues.push(PageWaitBlockingIssue::ParallelTaskUnresolved);
        return Ok(participation);
    }
    participation.reported_owners =
        parent.observation.wait.as_ref().map(|wait| wait.owners.clone()).unwrap_or_default();
    if !frame.context().has_qualified_local_owner() || native.epoch.is_none() {
        participation.issues.push(PageWaitBlockingIssue::NativeScopeUnqualified);
        return Ok(participation);
    }
    if parent.observation.blocking_status != ActivityBlockingStatus::Known {
        participation.issues.push(PageWaitBlockingIssue::WaitOwnerUnavailable);
        return Ok(participation);
    }
    add_contexts(&mut participation, parent.observation, frame);
    Ok(participation)
}

fn validate_source_binding(
    source: &CollectionDescription,
    page: &PageWaitObservation,
    original: &ActivityRequestFocus,
    frame: &ActivityFrameEvidence<'_>,
) -> Result<()> {
    PageWaitActivityEvidence::Focused { source, original, frame }.validate()?;
    if !source
        .observation
        .snapshot()
        .is_some_and(|snapshot| snapshot.accepts(TelemetryStream::OperationalSamples, &page.source))
        || !page.fits_source_scope(source)
        || ObservationBounds::new(&original.original_observed_at, &page.observed_from).is_none()
        || matches!(&source.observation, SourceObservation::FocusedPageWait { result, .. }
            if !result.accepts_observation(page))
        || page.request.key != original.original.key
        || page.request.statement_identity.as_ref() != Some(&original.statement_identity)
        || page.task_wait.is_none()
            && !original.original.wait.as_ref().is_some_and(|wait| {
                wait.identified_resources.contains(&page.resource)
                    && wait.native_type.as_deref()
                        == page.request.wait.as_ref().and_then(|wait| wait.native_type.as_deref())
            })
    {
        return Err(CollectionStop::InvalidPage.error());
    }
    Ok(())
}

fn add_contexts(
    participation: &mut PageWaitBlockingParticipation,
    waiter: &ActivityObservation,
    frame: &ActivityFrameEvidence<'_>,
) {
    let mut owners = BTreeSet::new();
    for owner in &participation.reported_owners {
        match owner {
            ActivityWaitOwner::Session { generation: None, .. } => {
                add_issue(&mut participation.issues, PageWaitBlockingIssue::OwnerGenerationUnavailable);
            }
            _ => {
                if let Some(session) = owner.session() {
                    owners.insert(session);
                } else {
                    add_issue(&mut participation.issues, PageWaitBlockingIssue::WaitOwnerUnavailable);
                }
            }
        }
    }
    if participation.reported_owners.is_empty() {
        add_issue(&mut participation.issues, PageWaitBlockingIssue::WaitOwnerUnavailable);
    }
    let mut contexts = Vec::new();
    for owner in owners {
        if owner == waiter.key.session {
            add_issue(&mut participation.issues, PageWaitBlockingIssue::SelfReference);
            continue;
        }
        let rows = frame.session_rows(&owner).collect::<Vec<_>>();
        if rows.is_empty() {
            add_issue(&mut participation.issues, PageWaitBlockingIssue::OwnerContextUnavailable);
        }
        if rows.iter().filter(|row| row.observation.key.request.is_some()).count() > 1 {
            add_issue(&mut participation.issues, PageWaitBlockingIssue::MultipleOwnerContexts);
        }
        participation.context_count += rows.len() as u32;
        if participation.context_count as usize <= MAX_PAGE_WAIT_BLOCKING_CONTEXTS {
            contexts.extend(rows.into_iter().map(|row| PageWaitBlockingContext {
                reference: reference(row),
                scope: row.observation.scope,
                state: row.observation.state.clone(),
                command: row.observation.command.clone(),
            }));
        }
    }
    if participation.context_count as usize > MAX_PAGE_WAIT_BLOCKING_CONTEXTS {
        add_issue(&mut participation.issues, PageWaitBlockingIssue::ContextsLimited);
    } else {
        participation.contexts = contexts;
    }
}

fn add_issue(issues: &mut Vec<PageWaitBlockingIssue>, issue: PageWaitBlockingIssue) {
    if !issues.contains(&issue) {
        issues.push(issue);
    }
}

fn reference(row: ActivityFrameRow<'_>) -> PageWaitActivityReference {
    PageWaitActivityReference {
        activity: row.observation.key.clone(),
        statement: row.observation.statement_identity.clone(),
        evidence: row.reference.cloned(),
    }
}

#[cfg(test)]
mod tests;
