//! Separately admitted, original-roster investigation. Ordinary page collection never calls this reader.
mod plan;
mod sql;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::replies as test_replies;
#[cfg(test)]
pub(crate) use tests::writer_focus as test_focus;

use super::*;
use crate::activity::{
    ActivityRecord, RequestLocator,
    participants::{self as requests, Check, RequestState},
};
use crate::{query_store::payload::Payload, read::integer};

const ENVELOPE_BYTES: usize = 4096;
const PROOF_BYTES_PER_CANDIDATE: usize = 4096;
const JSON_BYTES_PER_UTF16_BYTE: usize = 3;

pub(crate) struct PageWaitWriterCapture {
    pub context: NativeReadContext,
    pub observation: PageWaitWriterObservation,
    pub guard: PageWaitGuard,
}

struct RequestPoint {
    check: Check,
    record: Option<ActivityRecord>,
}

#[derive(Clone, Copy)]
struct PayloadAllowance {
    statement: usize,
    plan: usize,
}

impl<R: NativeRead> ActivityReader<R> {
    pub(crate) async fn capture_page_wait_writers(
        &mut self,
        focus: &PageWaitWriterFocus,
    ) -> Result<PageWaitWriterCapture> {
        validate_native_selectors(focus)?;
        let selected = self.capture_focused_page_wait_targets(&focus.waiter).await?;
        let captured = selected.capture;
        let context = NativeReadContext {
            target: captured.collection_target(),
            partition: captured.source_partition()?,
            observed_from: captured.snapshot.observed_from.clone(),
            observed_until: captured.snapshot.observed_until.clone(),
            database_writable: captured.context.writable,
        };
        let mut observation = PageWaitWriterObservation {
            observed_from: context.observed_from.clone(),
            observed_until: context.observed_until.clone(),
            focus_key: focus.key()?,
            target_state: initial_target_state(focus, captured.snapshot.observations.first()),
            page: captured.snapshot.observations.into_iter().next().map(Box::new),
            candidates: focus.candidates.iter().map(unverified).collect(),
        };
        if observation.target_state != PageWriterTargetState::CurrentMapped {
            return finish(focus, context, observation, captured.guard, &self.options.budget);
        }

        let before = self.writer_requests(focus, true).await?;
        populate_requests(focus, &before, &mut observation)?;
        let waiter = before.get(&focus.candidates.len()).ok_or_else(invalid_data)?;
        let checked = waiter.check.recheck_observation(&focus.waiter.participant().original, &focus.waiter.resource);
        if checked.request != PageWaitRequestState::SameStatement || checked.wait != PageWaitRelation::OriginalObserved
        {
            observation.target_state = PageWriterTargetState::NoLongerObserved;
            observation.page = None;
            let final_context = context::read(self, &captured.guard.0).await?;
            if !captured.guard.0.same_source(&final_context) {
                return Err(CollectionStop::SourceChanged.error());
            }
            let context = captured.guard.0.authority.completed(&final_context.authority)?;
            if !chronological(&[&observation.observed_until, &waiter.check.at, &context.observed_until]) {
                return Err(CollectionStop::SourceChanged.error());
            }
            observation.observed_until = context.observed_until.clone();
            return finish(focus, context, observation, captured.guard, &self.options.budget);
        }
        // Size metadata once, then share the remaining SQL/XML allowance across the complete roster.
        // A native plan which does not fit is unavailable, never truncated into a binding verdict.
        let cap = payload_allowance(&observation, &self.options.budget)?;
        if cap.statement == 0 || cap.plan == 0 {
            for candidate in &mut observation.candidates {
                if let Some(request) = &mut candidate.request {
                    request.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized };
                }
                candidate.plan = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized };
            }
        }
        if cap.statement > 0 && cap.plan > 0 && before.values().any(|point| point.record.is_some()) {
            self.admit_page_headers(&captured.guard.0).await?;
        }
        let payloads = self.writer_payloads(focus, &before, cap).await?;
        let after = self.writer_requests(focus, false).await?;
        qualify_candidates(focus, &context, &before, &after, payloads, &mut observation)?;

        // The current mapping and the original waiter must still be observed AFTER the selected plan reads.
        // Rechecking only the source process would attach contemporary writers to a vanished older wait.
        let address = PageAddress {
            database: focus.address.database_id,
            file: focus.address.file_id,
            page: focus.address.page_id,
        };
        self.admit_page_headers(&captured.guard.0).await?;
        let headers = self.page_headers(&BTreeSet::from([address]), &captured.guard.0).await?;
        let header = headers.get(&address).ok_or_else(invalid_data)?;
        // Echoed selector coordinates do not authenticate the independently decoded native header.
        if !address.matches(&header.header)? {
            return Err(invalid_data());
        }
        let final_context = context::read(self, &captured.guard.0).await?;
        if !captured.guard.0.same_source(&final_context) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let context = captured.guard.0.authority.completed(&final_context.authority)?;
        let waiter = after.get(&focus.candidates.len()).ok_or_else(invalid_data)?;
        let checked = waiter.check.recheck_observation(&focus.waiter.participant().original, &focus.waiter.resource);
        if !chronological(&[
            &observation.observed_until,
            &waiter.check.at,
            &header.observed_at,
            &context.observed_until,
        ]) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let current_target = mapped_target(&captured.guard.0, header)?;
        if checked.request != PageWaitRequestState::SameStatement || checked.wait != PageWaitRelation::OriginalObserved
        {
            observation.target_state = PageWriterTargetState::NoLongerObserved;
        } else {
            observation.target_state = target_state(focus, &current_target);
        }
        observation.observed_until = context.observed_until.clone();
        if observation.target_state == PageWriterTargetState::CurrentMapped {
            let page = observation.page.as_mut().ok_or_else(invalid_data)?;
            page.after_check = Some(checked);
            page.observed_until = header.observed_at.clone();
            page.target = Some(Box::new(current_target));
            // Extending the waiter check does not renew its earlier optional plan link. Preserve that link
            // only when the same native plan also agrees across the selected-writer read window.
            let first_waiter = before.get(&focus.candidates.len()).ok_or_else(invalid_data)?;
            let guarded_plan = first_waiter.check.identities(&waiter.check).and_then(|(_, plan)| plan);
            page.plan_identity = page.plan_identity.take().filter(|original| guarded_plan.as_ref() == Some(original));
        } else {
            observation.page = None;
            for candidate in &mut observation.candidates {
                candidate.relation = PageWriterRelation::Unavailable;
                candidate.binding = None;
            }
        }
        finish(focus, context, observation, captured.guard, &self.options.budget)
    }

    async fn writer_requests(
        &mut self,
        focus: &PageWaitWriterFocus,
        include_metadata: bool,
    ) -> Result<BTreeMap<usize, RequestPoint>> {
        let mut keys =
            focus.candidates.iter().enumerate().map(|(index, candidate)| (index, &candidate.key)).collect::<Vec<_>>();
        keys.push((keys.len(), &focus.waiter.participant().original.key));
        let query = sql::requests(include_metadata);
        let rows = self
            .io
            .query(&query, vec![Parameter::Text(requests::selector_keys(&keys)?)], keys.len())
            .await
            .map_err(ReadError::into_error)?;
        if rows.len() != keys.len() {
            return Err(invalid_data());
        }
        let mut points = BTreeMap::new();
        for row in &rows {
            let check = Check::decode(row)?;
            if check.ordinal >= keys.len() {
                return Err(invalid_data());
            }
            let record = if include_metadata
                && check.ordinal < focus.candidates.len()
                && check.selector_state(
                    &focus.candidates[check.ordinal].key,
                    &focus.candidates[check.ordinal].statement_identity,
                ) == RequestState::SameStatement
            {
                crate::activity::rows::record(row)?
            } else {
                None
            };
            if points.insert(check.ordinal, RequestPoint { check, record }).is_some() {
                return Err(invalid_data());
            }
        }
        Ok(points)
    }

    async fn writer_payloads(
        &mut self,
        focus: &PageWaitWriterFocus,
        before: &BTreeMap<usize, RequestPoint>,
        cap: PayloadAllowance,
    ) -> Result<BTreeMap<usize, (Payload, Payload)>> {
        let selectors = sql::payload_selectors(focus, before)?;
        if selectors.is_empty() || cap.statement == 0 || cap.plan == 0 {
            return Ok(BTreeMap::new());
        }
        let rows = self
            .io
            .query(
                &sql::payloads(),
                vec![
                    Parameter::Text(selectors),
                    Parameter::Integer(cap.statement as i64),
                    Parameter::Integer(cap.plan as i64),
                ],
                focus.candidates.len(),
            )
            .await
            .map_err(ReadError::into_error)?;
        let expected = focus
            .candidates
            .iter()
            .enumerate()
            .filter(|(ordinal, _)| {
                before.get(ordinal).is_some_and(|point| {
                    point.record.is_some() && point.check.identities(&point.check).and_then(|(_, plan)| plan).is_some()
                })
            })
            .count();
        if rows.len() != expected {
            return Err(invalid_data());
        }
        let mut payloads = BTreeMap::new();
        for row in &rows {
            let ordinal = usize::try_from(integer(row, "ordinal")?).map_err(|_| invalid_data())?;
            let expected = before.get(&ordinal).filter(|point| point.record.is_some()).ok_or_else(invalid_data)?;
            if ordinal >= focus.candidates.len()
                || expected.check.identities(&expected.check).and_then(|(_, plan)| plan).is_none()
            {
                return Err(invalid_data());
            }
            let statement = crate::cached_payload::read(row, "statement_bytes", "statement", cap.statement)?.0;
            let plan = crate::cached_payload::read(row, "plan_bytes", "plan", cap.plan)?.0;
            if payloads.insert(ordinal, (statement, plan)).is_some() {
                return Err(invalid_data());
            }
        }
        Ok(payloads)
    }
}

fn validate_native_selectors(focus: &PageWaitWriterFocus) -> Result<()> {
    if !focus.is_valid() {
        return Err(invalid_data());
    }
    if focus.waiter.participant().source != culprit_core::source_id::SESSIONS
        || PageAddress::parse(&focus.waiter.resource.native.id)
            != Some(PageAddress {
                database: focus.address.database_id,
                file: focus.address.file_id,
                page: focus.address.page_id,
            })
    {
        return Err(invalid_data());
    }
    for candidate in &focus.candidates {
        let request = candidate.key.request.as_ref().ok_or_else(invalid_data)?;
        for id in [&candidate.key.session.id, &request.id] {
            let native = id.parse::<u32>().map_err(|_| invalid_data())?;
            if native.to_string() != *id || native > i32::MAX as u32 {
                return Err(invalid_data());
            }
        }
        crate::sql_time::native_time(&candidate.key.session.generation)?;
        crate::sql_time::native_time(&request.generation)?;
        let locator = RequestLocator::from_identity(&candidate.statement_identity).ok_or_else(invalid_data)?;
        if u32::try_from(locator.database).ok() != Some(focus.address.database_id) {
            return Err(invalid_data());
        }
    }
    Ok(())
}

fn initial_target_state(focus: &PageWaitWriterFocus, page: Option<&PageWaitObservation>) -> PageWriterTargetState {
    let Some(page) = page else { return PageWriterTargetState::NoLongerObserved };
    if page.resolution != PageWaitResolution::ObservedAtBothBounds {
        return PageWriterTargetState::Unavailable;
    }
    if page.statement_identity.as_ref() != Some(&focus.waiter.participant().statement_identity) {
        return PageWriterTargetState::Unavailable;
    }
    if [&page.before_check, &page.after_check].into_iter().any(|check| {
        !check.as_ref().is_some_and(|check| {
            check.request == PageWaitRequestState::SameStatement && check.wait == PageWaitRelation::OriginalObserved
        })
    }) {
        return PageWriterTargetState::NoLongerObserved;
    }
    page.target.as_deref().map_or(PageWriterTargetState::Unavailable, |target| target_state(focus, target))
}

fn target_state(focus: &PageWaitWriterFocus, target: &MappedPageTarget) -> PageWriterTargetState {
    match target.writer_target_key() {
        Err(_) => PageWriterTargetState::Unavailable,
        Ok(current) if Some(&current) != focus.target.writer_target_key().ok().as_ref() => {
            PageWriterTargetState::ChangedTarget
        }
        Ok(_) => PageWriterTargetState::CurrentMapped,
    }
}

fn unverified(original: &PageWaitWriterRequest) -> PageWaitWriterCandidate {
    PageWaitWriterCandidate {
        parent: original.parent.clone(),
        request_state: PageWaitRequestState::Unverified,
        request: None,
        plan_identity: None,
        plan: unavailable(),
        relation: PageWriterRelation::Unavailable,
        binding: None,
    }
}
fn unavailable() -> DefinitionPayload {
    DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }
}
fn request_state(state: RequestState) -> PageWaitRequestState {
    match state {
        RequestState::SameStatement => PageWaitRequestState::SameStatement,
        RequestState::Missing => PageWaitRequestState::Missing,
        RequestState::Changed => PageWaitRequestState::Changed,
        RequestState::Unverified => PageWaitRequestState::Unverified,
    }
}

fn populate_requests(
    focus: &PageWaitWriterFocus,
    before: &BTreeMap<usize, RequestPoint>,
    observation: &mut PageWaitWriterObservation,
) -> Result<()> {
    for (index, (selected, current)) in focus.candidates.iter().zip(&mut observation.candidates).enumerate() {
        let point = before.get(&index).ok_or_else(invalid_data)?;
        current.request_state = request_state(point.check.selector_state(&selected.key, &selected.statement_identity));
        current.request = point.record.as_ref().map(|record| record.clone().into_redacted_observation());
    }
    Ok(())
}

fn payload_allowance(observation: &PageWaitWriterObservation, budget: &CollectionBudget) -> Result<PayloadAllowance> {
    let ceiling = (budget.page_bytes as usize).min(budget.total_bytes as usize);
    let metadata =
        culprit_core::serialization::json_size(observation, ceiling).map_err(|_| CollectionStop::ByteLimit.error())?;
    let remaining = ceiling
        .saturating_sub(metadata)
        .saturating_sub(ENVELOPE_BYTES)
        .saturating_sub(observation.candidates.len() * PROOF_BYTES_PER_CANDIDATE);
    let shared = remaining / (observation.candidates.len() * 2 * JSON_BYTES_PER_UTF16_BYTE);
    Ok(PayloadAllowance {
        statement: shared.min(budget.max_sql_bytes as usize),
        plan: shared.min(budget.max_plan_bytes as usize),
    })
}

fn qualify_candidates(
    focus: &PageWaitWriterFocus,
    context: &NativeReadContext,
    before: &BTreeMap<usize, RequestPoint>,
    after: &BTreeMap<usize, RequestPoint>,
    mut payloads: BTreeMap<usize, (Payload, Payload)>,
    observation: &mut PageWaitWriterObservation,
) -> Result<()> {
    for (index, (original, current)) in focus.candidates.iter().zip(&mut observation.candidates).enumerate() {
        let first = before.get(&index).ok_or_else(invalid_data)?;
        let last = after.get(&index).ok_or_else(invalid_data)?;
        if !chronological(&[&context.observed_until, &first.check.at, &last.check.at]) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let state = last.check.selector_state(&original.key, &original.statement_identity);
        if state != RequestState::SameStatement {
            current.request_state = request_state(state);
            current.request = None;
            continue;
        }
        if current.request.is_none() {
            continue;
        }
        let Some((_, Some(plan_identity))) = first.check.identities(&last.check) else { continue };
        let Some((statement, xml)) = payloads.remove(&index) else { continue };
        let request = current.request.as_mut().ok_or_else(invalid_data)?;
        request.statement = match &statement {
            Payload::Text(raw) => DefinitionPayload::sql(raw, culprit_core::Engine::Mssql),
            Payload::Unavailable(reason) => DefinitionPayload::Unavailable { reason: *reason },
        };
        current.plan = match &xml {
            Payload::Text(raw) => DefinitionPayload::plan(raw, culprit_core::Engine::Mssql),
            Payload::Unavailable(reason) => DefinitionPayload::Unavailable { reason: *reason },
        };
        let (Some(statement), Some(xml)) = (statement.text(), xml.text()) else { continue };
        let access = plan::classify(xml, statement, &focus.target, context)?;
        current.plan_identity = Some(plan_identity);
        if !current.plan.is_available()
            || !current.request.as_ref().is_some_and(|request| request.statement.is_available())
        {
            current.plan_identity = None;
            continue;
        }
        current.relation = access.relation;
        if !access.writes.is_empty() {
            current.binding = Some(MssqlPageWritePlanProof::new(
                &focus.target,
                current,
                digest(xml.as_bytes()),
                digest(statement.as_bytes()),
                access.writes,
            )?);
        }
    }
    Ok(())
}

fn finish(
    focus: &PageWaitWriterFocus,
    context: NativeReadContext,
    observation: PageWaitWriterObservation,
    guard: PageWaitGuard,
    budget: &CollectionBudget,
) -> Result<PageWaitWriterCapture> {
    if !focus.accepts_observation(&context, &observation) {
        return Err(invalid_data());
    }
    let ceiling = (budget.page_bytes as usize).min(budget.total_bytes as usize).saturating_sub(ENVELOPE_BYTES);
    culprit_core::serialization::json_size(&observation, ceiling).map_err(|_| CollectionStop::ByteLimit.error())?;
    Ok(PageWaitWriterCapture { context, observation, guard })
}
