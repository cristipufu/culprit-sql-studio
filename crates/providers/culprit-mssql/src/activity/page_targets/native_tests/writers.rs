//! Genuine native writer-plan acceptance remains separate from latch ownership and controlled causal work.
pub(super) mod workload;
use super::{
    durable_capture::{Captured, OwnedCaptures, require_readable_capture},
    fixture::{self, Fixture, invalid},
};
use culprit_core::{Result, WaitResourceKind, telemetry::*};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
use workload::Role;

const MAX_ORIGINAL_CAPTURES: usize = 4;

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn local_native_writer_plans_keep_targeted_writes_separate_from_readers_and_other_resources() {
    fixture::run(|fixture| Box::pin(accept_native_writers(fixture))).await;
}

async fn accept_native_writers(fixture: &mut Fixture) -> Result<()> {
    fixture.prepare_writer_pages().await?;
    fixture.start_other_target_gate().await?;
    fixture.begin_index_measurement().await?;
    let mut captures = OwnedCaptures::new(fixture)?;
    let started = fixture.start_writer_workload().await?;
    let mut outcomes = BTreeMap::<String, u32>::new();
    let mut positive = None;
    report_phase(started, "actors-started", 0, &outcomes);
    for attempt in 1..=MAX_ORIGINAL_CAPTURES {
        if fixture.writer_workload_finished() {
            break;
        }
        let original = captures.capture(CollectionSelection::Activity).await;
        report_phase(started, "original-activity-returned", attempt, &outcomes);
        let original = original?;
        require_readable_capture(&original)?;
        let Some(waiter) = original_waiter(&original)? else {
            *outcomes.entry("no-original-coordinator-memory-page-wait".into()).or_default() += 1;
            report_phase(started, "original-wait-unavailable", attempt, &outcomes);
            continue;
        };
        let page_capture =
            captures.capture(CollectionSelection::FocusedPageWait { focus: Box::new(waiter.clone()) }).await;
        report_phase(started, "focused-page-returned", attempt, &outcomes);
        let page_capture = page_capture?;
        require_readable_capture(&page_capture)?;
        let Some(page) = page(&page_capture)? else { continue };
        let reference = page_capture.reference(0)?;
        let plan = original.with_activity_frame(|frame| {
            plan_page_wait_writer_focus(page_capture.source()?, page, &reference, &waiter, frame)
        })?;
        let DiagnosticFocusPlan::Ready(planned) = plan else {
            *outcomes.entry("original-page-cannot-authorize-writer-question".into()).or_default() += 1;
            report_phase(started, "writer-question-unavailable", attempt, &outcomes);
            continue;
        };
        let DiagnosticFocus::PageWaitWriters { focus } = *planned else {
            return Err(invalid("native writer planner returned another question"));
        };
        original.with_activity_frame(|frame| {
            if !focus.matches_activity_frame(frame) {
                return Err(invalid("native writer focus lost its original authenticated roster"));
            }
            Ok(())
        })?;
        let captured = captures.capture(CollectionSelection::PageWaitWriters { focus: focus.clone() }).await;
        report_phase(started, "writer-source-returned", attempt, &outcomes);
        let captured = captured?;
        require_readable_capture(&captured)?;
        let current = writers(&captured, &focus)?;
        let qualified = qualifies_roles(&original, &focus, current, &mut outcomes);
        report_phase(started, "roles-checked", attempt, &outcomes);
        if qualified? {
            require_reuse_refusals(&original, &page_capture, &focus)?;
            positive = Some((original, page_capture, captured, focus));
            break;
        }
    }
    // The lock only keeps this independent control's original UPDATE alive across native capture. Release
    // it before disk verification or actor joins. Only this typed control records native 1222; every
    // ordinary actor error and every unexpected control error still fails through the retained owner.
    let released = fixture.release_holder_gate().await;
    report_phase(started, "wrong-target-gate-rollback-returned", 0, &outcomes);
    released?;
    if let Some((original, pages, captured, _)) = &positive {
        for retained in [original, pages, captured] {
            captures.verify_retained(retained).await?;
        }
    }
    let completed = fixture.finish_writer_workload().await;
    report_phase(started, "actors-completion-returned", 0, &outcomes);
    let completed = completed?;
    let window = fixture.finish_index_measurement().await?;
    let counts = workload::require_completions(&completed)?;
    if window.delta("page_fixture_hot", MssqlIndexCounter::LeafUpdates)? != counts.hot_updates
        || window.delta("page_fixture_split", MssqlIndexCounter::LeafUpdates)? != counts.other_updates
        || ["page_fixture_hot", "page_fixture_split", "page_fixture_select"]
            .into_iter()
            .any(|name| window.delta(name, MssqlIndexCounter::LeafInserts).ok() != Some(0))
    {
        return Err(invalid("native writer completion does not match the exact owned update/index populations"));
    }
    eprintln!(
        "{}",
        serde_json::json!({
            "phase":"owned-native-writer-acceptance","qualified_native_writer_source":positive.is_some(),
            "outcomes":outcomes,"completed_hot_update_rows":counts.hot_updates,"completed_other_update_rows":counts.other_updates,
            "other_target_native_lock_timeouts":counts.other_lock_timeouts,"other_target_attempts":counts.other_attempts,
            "actors":completed.len(),"counter_native_queries":window.queries(),
            "counter_read_work":window.metrics().summary(None)?,
            "direct_holder_proven":false,"query_causal_share_proven":false,"native_cost_qualified":false,
        })
    );
    let (original, pages, captured, focus) = positive.ok_or_else(|| invalid("bounded native writer producer did not qualify both writers and its independent reader/resource controls; no positive proof"))?;
    if original.activity_context()?.partition != window.context().partition
        || original.activity_context()?.target != window.context().target
    {
        return Err(invalid("native writer and counter source owners differ"));
    }
    let bounds = ObservationBounds::new(&window.context().observed_from, &window.context().observed_until)
        .ok_or_else(|| invalid("native writer counter window has invalid bounds"))?;
    let original_page = page(&pages)?.ok_or_else(|| invalid("native original mapped page was absent"))?;
    for retained in [&original, &pages, &captured] {
        let source = retained.source()?;
        let context = if source.observation.activity().is_some() {
            retained.activity_context()?
        } else {
            NativeReadContext::from_snapshot(source)?
        };
        let same_role = match &source.observation {
            SourceObservation::FocusedPageWait { result, .. } => {
                focused_role_matches(result, &focus.waiter, original_page, window.context().database_writable)
            }
            _ => context.database_writable == window.context().database_writable,
        };
        if context.target != window.context().target
            || context.partition != window.context().partition
            || !same_role
            || !bounds.contains(&context.observed_from)
            || !bounds.contains(&context.observed_until)
        {
            return Err(invalid("native writer evidence escaped its original source/role/counter interval"));
        }
    }
    // This real ended-request control preserves originals and cannot borrow a new request or current page.
    let ended = captures.capture(CollectionSelection::PageWaitWriters { focus: focus.clone() }).await?;
    require_readable_capture(&ended)?;
    let result = writers(&ended, &focus)?;
    if result.target_state == PageWriterTargetState::CurrentMapped
        || result.page.is_some()
        || result.candidates.iter().any(|row| row.binding.is_some() || row.relation != PageWriterRelation::Unavailable)
    {
        return Err(invalid("ended native writer actors acquired a current target or write binding"));
    }
    for retained in [&original, &pages, &captured, &ended] {
        captures.verify_retained(retained).await?;
    }
    Ok(())
}

fn focused_role_matches(
    result: &ActivityWaitFocusResult,
    original: &ActivityWaitFocus,
    page: &PageWaitObservation,
    counter_role: Option<bool>,
) -> bool {
    // The generic snapshot context retains no focused role. This exact authenticated Matched result records
    // the focused producer's native role comparison; it does not fill or default that generic context.
    result.focus == *original
        && result.permits_page_lookup()
        && result.accepts_observation(page)
        && result.focus.participant().database_writable.is_some_and(|role| Some(role) == counter_role)
}

fn report_phase(started: Instant, step: &str, attempt: usize, outcomes: &BTreeMap<String, u32>) {
    // This bounded host-clock diagnostic is fixture timing only, never native evidence or a rate.
    eprintln!(
        "{}",
        serde_json::json!({
            "phase":"owned-native-writer-lifecycle","step":step,"capture_attempt":attempt,
            "workload_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"role_outcomes":outcomes,
        })
    );
}

fn original_waiter(captured: &Captured) -> Result<Option<ActivityWaitFocus>> {
    let source = captured.source()?;
    let native = source.observation.activity().ok_or_else(|| invalid("native original Activity descriptor absent"))?;
    let context = captured.activity_context()?;
    if !context.has_qualified_local_owner() {
        return Err(invalid("native original Activity has no local owner"));
    }
    captured.with_activity_frame(|frame| {
        for (ordinal, row) in captured.records.iter().enumerate() {
            let TelemetryRecord::Activity(row) = &row.record else {
                return Err(invalid("native Activity changed stream"));
            };
            let role =
                row.client.as_ref().and_then(|client| client.application.as_deref()).and_then(Role::from_application);
            if !role.is_some_and(Role::is_target_writer)
                || row.command.as_deref() != Some("UPDATE")
                || !frame.has_session_generation_authority(&row.key.session)
            {
                continue;
            }
            let Some(statement) = &row.statement_identity else { continue };
            let Some(resource) = row.wait.as_ref().and_then(|wait| {
                wait.identified_resources.iter().find(|resource| resource.kind == WaitResourceKind::MemoryPage)
            }) else {
                continue;
            };
            let focus = ActivityWaitFocus {
                participant: ActivityRequestFocus {
                    partition: context.partition.clone(),
                    database_writable: context.database_writable,
                    parent: captured.reference(ordinal)?,
                    target: context.target.clone(),
                    source: source.source.clone(),
                    epoch: native.epoch.clone().ok_or_else(|| invalid("native original process epoch absent"))?,
                    original_observed_at: native.at.clone(),
                    statement_identity: statement.clone(),
                    original: row.as_ref().clone(),
                },
                resource: resource.clone(),
            };
            if !focus.is_valid() {
                return Err(invalid("native coordinator wait did not yield an authenticated focus"));
            }
            return Ok(Some(focus));
        }
        Ok(None)
    })
}

fn page(captured: &Captured) -> Result<Option<&PageWaitObservation>> {
    let Some(row) = captured.records.first() else { return Ok(None) };
    let TelemetryRecord::Operational(row) = &row.record else {
        return Err(invalid("native focused page changed stream"));
    };
    let OperationalFact::PageWait(page) = &row.fact else {
        return Err(invalid("native focused page changed fact"));
    };
    if captured.records.len() != 1 || page.task_wait.is_some() {
        return Err(invalid("native writer requires one genuine coordinator page"));
    }
    Ok(Some(page))
}

fn writers<'a>(captured: &'a Captured, focus: &PageWaitWriterFocus) -> Result<&'a PageWaitWriterObservation> {
    let context = NativeReadContext::from_snapshot(captured.source()?)?;
    let [row] = captured.records.as_slice() else {
        return Err(invalid("native writer source did not return one aggregate"));
    };
    let TelemetryRecord::Operational(row) = &row.record else {
        return Err(invalid("native writer source changed stream"));
    };
    let OperationalFact::PageWaitWriters(current) = &row.fact else {
        return Err(invalid("native writer source changed fact"));
    };
    if !focus.accepts_observation(&context, current) {
        return Err(invalid("native writer observation escaped its original focus and clock bounds"));
    }
    Ok(current)
}

fn qualifies_roles(
    original: &Captured,
    focus: &PageWaitWriterFocus,
    current: &PageWaitWriterObservation,
    outcomes: &mut BTreeMap<String, u32>,
) -> Result<bool> {
    *outcomes.entry(format!("target:{:?}", current.target_state)).or_default() += 1;
    if current.target_state != PageWriterTargetState::CurrentMapped {
        return Ok(false);
    }
    if focus.target.kind != PageWaitTargetKind::IndexLeaf
        || focus.target.object.as_ref().is_none_or(|object| {
            object.schema != "dbo"
                || object.name != "page_fixture_hot"
                || object.index_name.as_deref() != Some("PK_page_fixture_hot")
        })
    {
        return Err(invalid("native writer target is not its actual owned rowstore index leaf"));
    }
    let mut observed = BTreeSet::new();
    original.with_activity_frame(|frame| {
        for (selected, candidate) in focus.candidates.iter().zip(&current.candidates) {
            let row =
                frame.row(&selected.key).ok_or_else(|| invalid("native candidate lost its actual original row"))?;
            let Some(role) = row
                .observation
                .client
                .as_ref()
                .and_then(|client| client.application.as_deref())
                .and_then(Role::from_application)
            else {
                continue;
            };
            let command = if role.is_target_writer() || role == Role::OtherTarget { "UPDATE" } else { "SELECT" };
            if row.observation.command.as_deref() != Some(command) {
                continue;
            }
            *outcomes.entry(format!("{role:?}:{:?}", candidate.relation)).or_default() += 1;
            if candidate.request_state != PageWaitRequestState::SameStatement {
                continue;
            }
            let expected = if role.is_target_writer() {
                PageWriterRelation::TargetObjectWrite
            } else if role == Role::OtherTarget {
                PageWriterRelation::OtherTarget
            } else {
                PageWriterRelation::ReadOnlyPlan
            };
            if matches!(candidate.relation, PageWriterRelation::Unavailable | PageWriterRelation::Unsupported) {
                continue;
            }
            if candidate.relation != expected {
                return Err(invalid("native writer/read-only/other-resource verdict contradicted its original actor"));
            }
            if candidate
                .request
                .as_ref()
                .and_then(|row| row.client.as_ref())
                .and_then(|client| client.application.as_deref())
                != Some(role.application())
            {
                return Err(invalid("native candidate SQL received another original actor's application context"));
            }
            if role.is_target_writer()
                && candidate.binding.as_ref().is_none_or(|proof| {
                    proof.accesses.is_empty()
                        || proof.accesses.iter().any(|access| access.operation != PageWriteOperation::Update)
                })
            {
                return Err(invalid("native target writer did not retain its actual UPDATE operator proof"));
            }
            if !role.is_target_writer() && candidate.binding.is_some() {
                return Err(invalid("native reader or other-resource writer acquired a target write binding"));
            }
            observed.insert(role);
        }
        Ok(())
    })?;
    Ok([Role::Marker, Role::Payload, Role::TargetReader, Role::UnrelatedReader, Role::OtherTarget]
        .into_iter()
        .all(|role| observed.contains(&role)))
}

fn require_reuse_refusals(original: &Captured, pages: &Captured, focus: &PageWaitWriterFocus) -> Result<()> {
    original.with_activity_frame(|frame| {
        let mut reused = focus.clone();
        let candidate = &mut reused.candidates[0];
        candidate
            .key
            .request
            .as_mut()
            .ok_or_else(|| invalid("native original candidate has no request"))?
            .generation
            .push_str("-reused");
        candidate.parent.native_key = NativeRecordIdentity::Activity(candidate.key.clone());
        if !reused.is_valid() || reused.matches_activity_frame(frame) {
            return Err(invalid("a coherent recycled request claim bypassed the original frame receipt"));
        }
        let mut changed = focus.clone();
        changed.candidates[0].statement_identity.id.push_str("-changed");
        if !changed.is_valid() || changed.matches_activity_frame(frame) {
            return Err(invalid("a changed statement borrowed its earlier original request"));
        }
        Ok(())
    })?;
    let mut changed = focus.clone();
    changed.address.page_id += 1;
    changed.target.header.page_id += 1;
    if !changed.is_valid()
        || changed.matches_page(
            pages.source()?,
            page(pages)?.ok_or_else(|| invalid("native original page missing"))?,
            &pages.reference(0)?,
        )
    {
        return Err(invalid("another valid page address borrowed an original mapped-page receipt"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::page_targets::tests::{page_wait, reader, successful_replies};

    #[tokio::test]
    async fn focused_counter_role_requires_the_exact_qualified_result_without_filling_generic_context() {
        let writer = crate::activity::page_targets::writers::test_focus().await;
        let mut owner = reader(successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]));
        let focused = owner.capture_focused_page_wait_targets(&writer.waiter).await.unwrap();
        assert_eq!(owner.close().await, SourceShutdown::Quiescent);
        let page = &focused.capture.snapshot.observations[0];
        let result = focused.result;
        let source = CollectionDescription {
            target: focused.capture.collection_target(),
            partition: focused.capture.source_partition().unwrap(),
            source: crate::sources::PAGE_WAIT_TARGETS.name.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::FocusedPageWait {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageWaitTargets,
                    observed_from: focused.capture.snapshot.observed_from.clone(),
                    observed_until: focused.capture.snapshot.observed_until.clone(),
                    records: 1,
                    sources: vec![StateSourceCoverage::observed(
                        crate::sources::PAGE_WAIT_TARGETS.name.into(),
                        1,
                        false,
                    )],
                },
                result: Box::new(result.clone()),
            },
        };
        let generic = NativeReadContext::from_snapshot(&source).unwrap();
        assert_eq!(generic.database_writable, None);
        assert!(focused_role_matches(&result, &writer.waiter, page, Some(true)));
        assert_eq!(generic.database_writable, None, "focused admission never fills the generic role");
        assert!(!focused_role_matches(&result, &writer.waiter, page, None));
        assert!(!focused_role_matches(&result, &writer.waiter, page, Some(false)));

        let mut foreign = result.clone();
        foreign.focus.participant_mut().parent.record_id = digest(b"another-original-page-wait");
        assert!(foreign.is_valid());
        assert!(!focused_role_matches(&foreign, &writer.waiter, page, Some(true)));
        for state in [FocusSourceState::Changed, FocusSourceState::Unverified] {
            let mut unavailable = result.clone();
            unavailable.source = state;
            unavailable.request = PageWaitRequestState::Unverified;
            unavailable.wait = PageWaitRelation::Unqualified;
            assert!(unavailable.is_valid());
            assert!(!focused_role_matches(&unavailable, &writer.waiter, page, Some(true)));
        }
        for role in [None, Some(false)] {
            let mut changed = result.clone();
            changed.focus.participant_mut().database_writable = role;
            let matching_original = changed.focus.clone();
            assert!(!focused_role_matches(&changed, &matching_original, page, Some(true)));
        }
    }
}
