//! An existing incident can justify one bounded native task population from its exact earlier Activity frame.
//! The incident request is a trigger, not a claim that every observed task belongs to that request.
use super::*;

/// Native actors from one original Activity capture, without SQL, client details or copied counter bodies.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActivityRequestSelector {
    pub key: ActivityKey,
    pub statement_identity: NativeIdentity,
}
impl ActivityRequestSelector {
    pub fn is_valid(&self) -> bool {
        self.key.is_valid() && self.key.request.is_some() && self.statement_identity.is_valid()
    }

    /// A canonical full roster has one statement token per exact native request generation.
    pub fn is_canonical(requests: &[Self]) -> bool {
        !requests.is_empty()
            && requests.len() <= MAX_ACTIVITY_FRAME_OBSERVATIONS
            && requests.iter().all(Self::is_valid)
            && requests.windows(2).all(|pair| pair[0].key < pair[1].key)
    }
}

impl ActivityFrameEvidence<'_> {
    /// The caller authenticates the frame first. Every eligible retained request is included; capacity policy
    /// may refuse the resulting request, but cannot silently replace the roster with a prefix or one trigger.
    pub fn page_wait_task_requests(&self) -> Result<Vec<ActivityRequestSelector>> {
        if !self.read_complete() {
            return Err(CollectionStop::InvalidPage.error());
        }
        let mut requests = Vec::new();
        for row in self.rows() {
            let observation = row.observation;
            if observation.scope != ActivityScopeBasis::RequestDatabase || observation.key.request.is_none() {
                continue;
            }
            let Some(statement_identity) = &observation.statement_identity else { continue };
            if row.reference.is_none() {
                return Err(CollectionStop::InvalidPage.error());
            }
            requests.push(ActivityRequestSelector {
                key: observation.key.clone(),
                statement_identity: statement_identity.clone(),
            });
        }
        Ok(requests)
    }
}

impl ActivityRequestFocus {
    pub fn is_page_wait_tasks_focus(&self, context: &NativeReadContext) -> bool {
        self.is_valid()
            && self.original.scope == ActivityScopeBasis::RequestDatabase
            && context.has_qualified_local_owner()
            && context.target.database().is_some()
            && context.database_writable.is_some()
            && context.target == self.target
            && context.partition == self.partition
            && context.database_writable == self.database_writable
            && ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.original_observed_at))
    }
}

impl DiagnosticEpisode {
    pub fn matches_page_wait_tasks_focus(&self, focus: &ActivityRequestFocus, context: &NativeReadContext) -> bool {
        focus.parent == self.first
            && self.matches_request_origin(focus)
            && focus.is_page_wait_tasks_focus(context)
            && matches!(self.signal, DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. })
    }

    /// Callers authenticate the original capture and use the selected source catalog to establish which next
    /// question exists. This pure relationship cannot grant native work or infer a coordinator's wait type.
    pub fn plan_page_wait_tasks_focus(&self, frame: &ActivityFrameEvidence<'_>) -> Result<DiagnosticFocusPlan> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        if !self.analysis.needs_work()
            || !matches!(self.signal, DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. })
        {
            return Ok(DiagnosticFocusPlan::NotRequired);
        }
        if !frame.read_complete() {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::EvidenceUnavailable));
        }
        let Some(original) = frame.row(self.signal.activity_key()) else {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        };
        let Some(reference) = original.reference else {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        };
        if *reference != self.first {
            return Err(CollectionStop::InvalidPage.error());
        }
        let Some(focus) = self.original_request_focus(frame.source(), original.observation, reference)? else {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        };
        let context = frame.context();
        if !focus.is_page_wait_tasks_focus(&context) {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        }
        if !self.matches_page_wait_tasks_focus(&focus, &context) {
            return Err(CollectionStop::InvalidPage.error());
        }
        let original_requests = frame.page_wait_task_requests()?;
        Ok(DiagnosticFocusPlan::Ready(Box::new(DiagnosticFocus::PageWaitTasks {
            focus: Box::new(focus),
            original_context: Box::new(context),
            original_requests,
        })))
    }
}
