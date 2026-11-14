//! Page-specific wait rechecks use the shared native request/statement qualification.
use super::super::{
    ActivityRecord,
    participants::{Check, RequestState},
};
use culprit_core::{
    ObservedWaitResource,
    telemetry::{ActivityObservation, PageWaitRecheck, PageWaitRelation, PageWaitRequestState},
};

impl Check {
    pub(super) fn recheck(&self, record: &ActivityRecord, resource: &ObservedWaitResource) -> PageWaitRecheck {
        self.recheck_request(self.request_state(record), &record.facts, resource)
    }

    pub(super) fn recheck_observation(
        &self,
        original: &ActivityObservation,
        resource: &ObservedWaitResource,
    ) -> PageWaitRecheck {
        self.recheck_request(self.observation_state(original), original, resource)
    }

    fn recheck_request(
        &self,
        state: RequestState,
        original: &ActivityObservation,
        resource: &ObservedWaitResource,
    ) -> PageWaitRecheck {
        let request = match state {
            RequestState::SameStatement => PageWaitRequestState::SameStatement,
            RequestState::Missing => PageWaitRequestState::Missing,
            RequestState::Changed => PageWaitRequestState::Changed,
            RequestState::Unverified => PageWaitRequestState::Unverified,
        };
        let wait = if request == PageWaitRequestState::SameStatement {
            match self.wait_type.as_deref() {
                None => PageWaitRelation::NoLongerObserved,
                Some(native) if Some(native) != original.wait.as_ref().and_then(|wait| wait.native_type.as_deref()) => {
                    PageWaitRelation::Different
                }
                Some(_) => match self.resource.as_deref().filter(|resource| !resource.is_empty()) {
                    Some(current) if current == resource.native.id => PageWaitRelation::OriginalObserved,
                    Some(_) => PageWaitRelation::Different,
                    None => PageWaitRelation::Unqualified,
                },
            }
        } else {
            PageWaitRelation::Unqualified
        };
        PageWaitRecheck { observed_at: self.at.clone(), request, wait }
    }
}
