//! A focused followup admits only a still-observed original statement and wait before reading page metadata.
use super::*;
use crate::activity::{RequestLocator, participants::Check};

pub(crate) struct FocusedPageWaitCapture {
    pub capture: PageWaitCapture,
    pub result: ActivityWaitFocusResult,
}

impl<R: NativeRead> ActivityReader<R> {
    pub(crate) async fn capture_focused_page_wait_targets(
        &mut self,
        focus: &ActivityWaitFocus,
    ) -> Result<FocusedPageWaitCapture> {
        if !focus.is_valid()
            || focus.source != culprit_core::source_id::SESSIONS
            || RequestLocator::from_identity(&focus.statement_identity).is_none()
            || !matches!(self.options.scope, ActivityScope::CurrentDatabase)
        {
            return Err(invalid_data());
        }
        let initial = context::initial(self).await?;
        let current = initial.capture_context()?;
        if current.database != *focus.target.database().ok_or_else(invalid_data)?
            || current.epoch.as_deref() != Some(focus.epoch.as_str())
            || !chronological(&[&canonical_time(&focus.original_observed_at)?, &current.at])
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let session = native_id(&focus.original.key.session.id)?;
        let request = native_id(&focus.original.key.request.as_ref().ok_or_else(invalid_data)?.id)?;
        let frame = self
            .read_frame(
                &sql::focused_observation(self.options.identity),
                vec![Parameter::Integer(1), Parameter::Integer(session), Parameter::Integer(request)],
            )
            .await?;
        if !initial.matches_initial(&frame) || frame.records.len() > 1 {
            return Err(CollectionStop::SourceChanged.error());
        }
        let checked = frame.records.first().map(|record| Check::from_record(record, &frame.context.at));
        let recheck = checked.map(|check| check.recheck_observation(&focus.original, &focus.resource));
        let (request, wait) =
            recheck.as_ref().map_or((PageWaitRequestState::Missing, PageWaitRelation::Unqualified), |check| {
                (check.request, check.wait)
            });
        let mut result = ActivityWaitFocusResult {
            lineage: None,
            focus: focus.clone(),
            observed_from: current.at,
            observed_until: frame.context.at.clone(),
            source: FocusSourceState::Matched,
            request,
            wait,
        };
        let capture = if result.permits_page_lookup() {
            self.capture_selected_page_wait_targets(frame, initial, LookupAdmission::OriginalFocus).await?
        } else {
            // Preserve the authenticated original in result. A changed or disappeared participant is not a
            // fresh page observation, and must never trigger a broad replacement population or a header read.
            let frame = ActivityFrame { context: frame.context, records: Vec::new() };
            self.finish_page_capture(frame, Vec::new(), false, initial).await?
        };
        result.observed_until = capture.snapshot.observed_until.clone();
        if let Some(checked) =
            capture.snapshot.observations.first().and_then(|row| row.after_check.as_ref().or(row.before_check.as_ref()))
        {
            result.request = checked.request;
            result.wait = checked.wait;
        }
        if !result.is_valid() {
            return Err(invalid_data());
        }
        Ok(FocusedPageWaitCapture { capture, result })
    }
}

fn native_id(value: &str) -> Result<i64> {
    let id: i32 = value.parse().map_err(|_| invalid_data())?;
    if id < 0 || id.to_string() != value {
        return Err(invalid_data());
    }
    Ok(i64::from(id))
}

#[cfg(test)]
pub(crate) mod tests;
