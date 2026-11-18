//! Explicit original-task investigation. Ordinary activity/page/writer reads never invoke this profile.
mod decode;
mod sql;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::{focus as test_focus, replies as test_replies};

use super::*;
use crate::activity::{ActivityRecord, NativeStatement, RequestLocator, participants::Check};
use decode::HolderRead;

const HOLDER_POINT_ROWS: usize = (MAX_PAGE_TASK_HOLDER_ROWS as usize) * 2 + 1;
const ENVELOPE_BYTES: usize = 4096;
const JSON_BYTES_PER_UTF16_BYTE: usize = 3;
const MAX_HOLDER_QUERIES: u32 = 28;

/// Existing page safety limits plus finite selected-holder metadata/SQL and final page/native-owner checks.
/// This is an admission ceiling, not a measured cost qualification or a default collection route.
pub(crate) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    let mut budget = crate::page_waits::bounded_budget(requested);
    budget.max_queries = requested.max_queries.min(MAX_HOLDER_QUERIES);
    budget.max_snapshot_rows = budget.max_snapshot_rows.min(1);
    budget.page_rows = budget.page_rows.min(1);
    budget
}

pub(crate) struct PageTaskHolderCapture {
    pub context: NativeReadContext,
    pub observation: PageTaskHolderObservation,
    pub guard: PageWaitGuard,
    pub task_visibility_unavailable: bool,
    pub optional_sql_error: Option<ErrorCode>,
}

impl<R: NativeRead> ActivityReader<R> {
    /// The caller publishes only after the existing native reader confirms close. Point relationships are
    /// never an atomic latch-owner snapshot or permission to retry on another pooled connection.
    pub(crate) async fn capture_page_task_holders(
        &mut self,
        focus: &PageTaskHolderFocus,
    ) -> Result<PageTaskHolderCapture> {
        validate_native_focus(focus)?;
        if self.observed {
            return Err(invalid_data());
        }
        self.observed = true;
        let initial = context::initial(self).await?;
        validate_original_context(focus, &initial)?;
        let mut before = self.page_holder_point(focus).await?;
        if !initial.matches_initial(&before.frame) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let checked = before.recheck(focus);
        let captured = if checked.request == PageWaitRequestState::SameStatement
            && checked.wait == PageWaitRelation::OriginalObserved
            && before.task.is_some()
        {
            self.capture_selected_page_wait_targets(
                task_waits::Population {
                    frame: ActivityFrame {
                        context: before.frame.context.clone(),
                        records: before.frame.records.clone(),
                    },
                    tasks: vec![before.task.clone()],
                    limited: false,
                    task_visibility: Some(before.visible),
                },
                initial,
                LookupAdmission::OriginalFocus,
            )
            .await?
        } else {
            self.finish_page_capture(
                ActivityFrame { context: before.frame.context.clone(), records: Vec::new() },
                Vec::new(),
                false,
                initial,
            )
            .await?
        };
        let mut target_state = if before.visible {
            target_state(focus, captured.snapshot.observations.first())
        } else {
            PageWriterTargetState::Unavailable
        };
        let page = captured.snapshot.observations.into_iter().next().map(Box::new);
        let first = before.model_for_page(focus, page.as_deref())?;
        let mut provisional = PageTaskHolderObservation {
            observed_from: captured.snapshot.observed_from.clone(),
            observed_until: captured.snapshot.observed_until.clone(),
            focus_key: focus.key()?,
            target_state,
            page: page.clone(),
            final_target: None,
            before: first.clone(),
            after_task: before.task.clone(),
            after: first,
        };
        let mut optional_sql_error = None;
        if target_state == PageWriterTargetState::CurrentMapped
            && provisional.before.holder.relation == PageTaskHolderRelation::QualifiedRequest
        {
            let cap = statement_allowance(&provisional, &self.options.budget)?;
            optional_sql_error = self.enrich_holder(&mut before, cap).await?;
        }
        let final_target = if target_state == PageWriterTargetState::CurrentMapped {
            Some(self.recheck_holder_target(focus, &captured.guard).await?)
        } else {
            None
        };
        if let Some((state, _, _)) = &final_target {
            target_state = *state;
        }
        let after = self.page_holder_point(focus).await?;
        if !captured.guard.0.matches_initial(&after.frame) || before.visible != after.visible {
            return Err(CollectionStop::SourceChanged.error());
        }
        let final_check = after.recheck(focus);
        target_state = final_target_state(target_state, &final_check);
        let final_context = context::read(self, &captured.guard.0).await?;
        if !captured.guard.0.same_source(&final_context) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let context = captured.guard.0.authority.completed(&final_context.authority)?;
        if !chronological(&[
            &context.observed_from,
            &before.frame.context.at,
            &captured.snapshot.observed_until,
            &after.frame.context.at,
            &context.observed_until,
        ]) {
            return Err(CollectionStop::SourceChanged.error());
        }
        if final_target
            .as_ref()
            .is_some_and(|(_, _, at)| !chronological(&[&captured.snapshot.observed_until, at, &after.frame.context.at]))
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let mut last = after.model(focus)?;
        // Reuse full SQL only for exactly the same native request/statement. A different holder remains a
        // separately identified point with an unavailable SQL payload, never the first holder's text.
        if let (Some(first), Some(last)) = (&before.owner, &mut last.holder.request)
            && first.facts.key == last.key
            && first.facts.statement_identity == last.statement_identity
        {
            last.statement = first.clone().into_redacted_observation().statement;
        }
        provisional.observed_from = context.observed_from.clone();
        provisional.observed_until = context.observed_until.clone();
        provisional.page = page;
        provisional.final_target = final_target.and_then(|(_, target, observed_at)| {
            target.map(|target| Box::new(PageTaskHolderTargetCheck { observed_at, target: Box::new(target) }))
        });
        provisional.target_state = target_state;
        provisional.before = before.model_for_page(focus, provisional.page.as_deref())?;
        provisional.after_task = after.task;
        provisional.after = last;
        if !focus.accepts_observation(&context, &provisional) {
            return Err(invalid_data());
        }
        ensure_retained_size(&provisional, &self.options.budget)?;
        Ok(PageTaskHolderCapture {
            context,
            observation: provisional,
            guard: captured.guard,
            task_visibility_unavailable: !before.visible,
            optional_sql_error,
        })
    }

    async fn page_holder_point(&mut self, focus: &PageTaskHolderFocus) -> Result<HolderRead> {
        let rows = self
            .io
            .query(
                &sql::point(self.options.identity, self.request_page_server_read_capability),
                vec![
                    Parameter::Integer(HOLDER_POINT_ROWS as i64),
                    Parameter::Integer(i64::from(focus.task.session_id)),
                    Parameter::Integer(i64::from(focus.task.request_id)),
                    Parameter::Text(focus.task.task_address.clone()),
                    Parameter::Integer(i64::from(focus.task.execution_context_id)),
                ],
                HOLDER_POINT_ROWS,
            )
            .await
            .map_err(ReadError::into_error)?;
        HolderRead::decode(&rows, focus, &self.host, self.port, self.options.identity)
    }

    async fn enrich_holder(&mut self, point: &mut HolderRead, cap: u32) -> Result<Option<ErrorCode>> {
        let Some(record) = point.owner.as_ref() else { return Ok(None) };
        if cap == 0 {
            point.owner.as_mut().ok_or_else(invalid_data)?.current_sql =
                NativeStatement::Unavailable(PayloadUnavailable::Oversized);
            return Ok(None);
        }
        let key = record.facts.key.clone();
        let mut frame = ActivityFrame { context: point.frame.context.clone(), records: vec![record.clone()] };
        let status = self
            .enrich_using_policy(
                &mut frame,
                &BTreeSet::from([key]),
                false,
                cap,
                crate::activity::sql::QueryPolicy::SerialMetadata,
            )
            .await?;
        point.owner = frame.records.into_iter().next();
        Ok(match status {
            ActivityDetailStatus::Unavailable { code } => Some(code),
            _ => None,
        })
    }

    async fn recheck_holder_target(
        &mut self,
        focus: &PageTaskHolderFocus,
        guard: &PageWaitGuard,
    ) -> Result<(PageWriterTargetState, Option<MappedPageTarget>, String)> {
        let address = PageAddress {
            database: focus.address.database_id,
            file: focus.address.file_id,
            page: focus.address.page_id,
        };
        self.admit_page_headers(&guard.0).await?;
        let rows = self.page_headers(&BTreeSet::from([address]), &guard.0).await?;
        let header = rows.get(&address).ok_or_else(invalid_data)?;
        if !address.matches(&header.header)? {
            return Err(invalid_data());
        }
        let current = mapped_target(&guard.0, header)?;
        let Ok(key) = current.writer_target_key() else {
            return Ok((PageWriterTargetState::Unavailable, Some(current), header.observed_at.clone()));
        };
        let state = if focus.target.writer_target_key()? == key {
            PageWriterTargetState::CurrentMapped
        } else {
            PageWriterTargetState::ChangedTarget
        };
        Ok((state, Some(current), header.observed_at.clone()))
    }
}

fn validate_native_focus(focus: &PageTaskHolderFocus) -> Result<()> {
    if !focus.is_valid() || RequestLocator::from_identity(&focus.participant.statement_identity).is_none() {
        return Err(invalid_data());
    }
    crate::sql_time::native_time(&focus.participant.original.key.session.generation)?;
    crate::sql_time::native_time(
        &focus.participant.original.key.request.as_ref().ok_or_else(invalid_data)?.generation,
    )?;
    Ok(())
}

fn validate_original_context(focus: &PageTaskHolderFocus, initial: &context::LookupContext) -> Result<()> {
    let current = initial.capture_context()?;
    if current.database != *focus.participant.target.database().ok_or_else(invalid_data)?
        || initial.authority.source_partition()? != focus.participant.partition
        || current.epoch.as_deref() != Some(focus.participant.epoch.as_str())
        || current.writable != focus.participant.database_writable
        || !chronological(&[&canonical_time(&focus.original_observed_until)?, &current.at])
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(())
}

fn target_state(focus: &PageTaskHolderFocus, page: Option<&PageWaitObservation>) -> PageWriterTargetState {
    let Some(page) = page else { return PageWriterTargetState::NoLongerObserved };
    let Some(target) = &page.target else { return PageWriterTargetState::Unavailable };
    match target.writer_target_key() {
        Ok(key) if focus.target.writer_target_key().as_ref().ok() == Some(&key) => PageWriterTargetState::CurrentMapped,
        Ok(_) => PageWriterTargetState::ChangedTarget,
        Err(_) => PageWriterTargetState::Unavailable,
    }
}

fn final_target_state(mapped: PageWriterTargetState, check: &PageWaitRecheck) -> PageWriterTargetState {
    use PageWaitRelation::{Different, NoLongerObserved, OriginalObserved, Unqualified};
    use PageWaitRequestState::{Changed, Missing, SameStatement, Unverified};
    match (mapped, check.request, check.wait) {
        (PageWriterTargetState::CurrentMapped, SameStatement, OriginalObserved) => mapped,
        (PageWriterTargetState::CurrentMapped, Missing | Changed, _)
        | (PageWriterTargetState::CurrentMapped, SameStatement, Different | NoLongerObserved) => {
            PageWriterTargetState::NoLongerObserved
        }
        (PageWriterTargetState::CurrentMapped, _, _)
        | (PageWriterTargetState::NoLongerObserved, Unverified, _)
        | (PageWriterTargetState::NoLongerObserved, SameStatement, OriginalObserved | Unqualified) => {
            PageWriterTargetState::Unavailable
        }
        _ => mapped,
    }
}

fn record_limit(budget: &CollectionBudget) -> usize {
    budget.page_bytes.min(budget.total_bytes) as usize
}

fn statement_allowance(row: &PageTaskHolderObservation, budget: &CollectionBudget) -> Result<u32> {
    let limit = record_limit(budget);
    let size = culprit_core::serialization::json_size(row, limit).map_err(|_| CollectionStop::ByteLimit.error())?;
    let mapping = row.page.as_ref().and_then(|page| page.target.as_ref());
    let mapping_bytes = mapping
        .map(|target| culprit_core::serialization::json_size(target, limit))
        .transpose()
        .map_err(|_| CollectionStop::ByteLimit.error())?
        .unwrap_or(0);
    let remaining = limit.saturating_sub(size).saturating_sub(mapping_bytes).saturating_sub(ENVELOPE_BYTES);
    // The same SQL may be retained at both bounds. UTF-16 native bytes can expand through JSON escaping.
    Ok((remaining / (JSON_BYTES_PER_UTF16_BYTE * 2)).min(MAX_SQL_BYTES as usize).min(budget.max_sql_bytes as usize)
        as u32)
}

fn ensure_retained_size(row: &PageTaskHolderObservation, budget: &CollectionBudget) -> Result<()> {
    culprit_core::serialization::json_size(row, record_limit(budget))
        .map(|_| ())
        .map_err(|_| CollectionStop::ByteLimit.error())
}
