//! Page-specific wait rechecks use the shared native request/statement qualification.
use super::super::{
    ActivityRecord,
    participants::{Check, RequestState},
};
use culprit_core::{
    ObservedWaitResource,
    telemetry::{PageWaitRecheck, PageWaitRelation, PageWaitRequestState},
};

impl Check {
    pub(super) fn recheck(&self, record: &ActivityRecord, resource: &ObservedWaitResource) -> PageWaitRecheck {
        let request = match self.request_state(record) {
            RequestState::SameStatement => PageWaitRequestState::SameStatement,
            RequestState::Missing => PageWaitRequestState::Missing,
            RequestState::Changed => PageWaitRequestState::Changed,
            RequestState::Unverified => PageWaitRequestState::Unverified,
        };
        let wait = if request == PageWaitRequestState::SameStatement {
            match self.wait_type.as_deref() {
                None => PageWaitRelation::NoLongerObserved,
                Some(native)
                    if Some(native) != record.facts.wait.as_ref().and_then(|wait| wait.native_type.as_deref()) =>
                {
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
