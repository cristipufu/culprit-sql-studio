//! One owned native scenario serves compatibility and strict positive qualification. Native task evidence
//! can establish compatibility while missing/transient holder fields keep the causal qualification open.
use super::{
    durable_capture::{Captured, OwnedCaptures},
    fixture::{self, Fixture, invalid},
    workload::{HOLDER_ROWS_PER_WORKER, WRITER_APPLICATIONS, WRITERS_PER_PATTERN},
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{Result, telemetry::*};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    time::Duration,
};

mod acceptance;
mod identity_receipt;
#[cfg(test)]
mod test_support;
use acceptance::{NativeHolderEvidence, require_readable_capture};

const MAX_CAPTURE_ATTEMPTS: usize = 8;
const MAX_PLANNER_DIAGNOSTIC_SAMPLES: u32 = 4;
const ORIGINAL_GATE_TIMEOUT: Duration = Duration::from_secs(3);

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn an_owned_native_page_task_holder_requires_the_reported_pointer_and_exact_current_request() {
    fixture::run(|fixture| Box::pin(async move { observe_owned_holder(fixture).await?.require_qualified_holder() }))
        .await;
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn owned_native_page_tasks_preserve_original_receipts_and_truthful_holder_refusals() {
    fixture::run(|fixture| Box::pin(async move { observe_owned_holder(fixture).await?.require_compatibility() })).await;
}

async fn observe_owned_holder(fixture: &mut Fixture) -> Result<NativeHolderEvidence> {
    let mut captures = OwnedCaptures::new(fixture)?;
    fixture.start_holder_gate().await?;
    let work = fixture.start_holder_workload().await?;
    // The source is drained and closed while these exact INSERTs remain blocked on the owned seed table.
    // Releasing that lock changes their wait, not their original request/statement authority.
    let (original, mut query_count) = capture_gated_original(&mut captures).await?;
    let activity = original.capture_ref()?;
    let original_context = Box::new(original.activity_context()?);
    let original_requests = original.with_activity_frame(|frame| frame.page_wait_task_requests())?;
    captures.verify_retained(&original).await?;
    fixture.release_holder_gate().await?;
    let released_at = std::time::Instant::now();
    let gate_held_client_ms = work.elapsed().as_secs_f64() * 1000.0;
    let mut outcomes = BTreeMap::new();
    let mut planning_outcomes = BTreeMap::new();
    let mut evidence = NativeHolderEvidence::default();
    for attempt in 0..MAX_CAPTURE_ATTEMPTS {
        if fixture.workers_finished() {
            break;
        }
        let pages = captures
            .capture(CollectionSelection::PageWaitTasks {
                activity: activity.clone(),
                original_context: original_context.clone(),
                original_requests: original_requests.clone(),
            })
            .await?;
        query_count += pages.manifest.source_queries;
        require_readable_capture(&pages)?;
        if !matches!(&pages.source()?.observation,
            SourceObservation::PageWaitTargets { original_activity: Some(parent), .. } if *parent == activity)
        {
            return Err(invalid("native task capture lost its exact original Activity parent"));
        }
        captures.verify_retained(&original).await?;
        captures.verify_retained(&pages).await?;
        let mut selected = None;
        for ordinal in 0..pages.records.len() {
            let page = captured_page(&pages, ordinal)?;
            evidence.observe_page(page, &original_requests)?;
            let focus = original_focus(&original, &pages, ordinal, page, &mut planning_outcomes)?;
            if selected.is_none() {
                selected = focus;
            }
        }
        let Some(focus) = selected else { continue };
        let (points, queries) = inspect_holder(&mut captures, &focus, attempt, &mut outcomes).await?;
        captures.verify_retained(&original).await?;
        captures.verify_retained(&pages).await?;
        query_count += queries;
        evidence.observe_qualified_points(points);
        if evidence.has_qualified_holder() {
            break;
        }
    }
    let completed = fixture.finish_workload().await?;
    if completed.len() != WRITERS_PER_PATTERN * 2
        || completed.iter().any(|worker| worker.inserts != HOLDER_ROWS_PER_WORKER as u64)
    {
        return Err(invalid("owned single-INSERT workers did not complete their exact bounded row population"));
    }
    eprintln!(
        "{}",
        serde_json::json!({
            "phase": "owned-holder-qualification-summary", "native_queries": query_count,
            "native_holder_relations": outcomes, "native_positive": evidence.has_qualified_holder(),
            "native_task_waits": evidence.native_task_waits(),
            "native_compatibility_evidence": evidence.require_compatibility().is_ok(),
            "holder_qualification_open": !evidence.has_qualified_holder(),
            "contention_cause_proven": false, "continuous_owner_proven": false,
            "original_planning_outcomes": planning_outcomes,
            "fixture_work_client_ms": work.elapsed().as_secs_f64() * 1000.0,
            "fixture_gate_held_client_ms": gate_held_client_ms,
            "fixture_after_gate_client_ms": released_at.elapsed().as_secs_f64() * 1000.0,
            "original_activity_reused": activity,
            "completed_fixture_inserts": completed.iter().map(|worker| worker.inserts).sum::<u64>(),
            "fixture_rows_per_worker": HOLDER_ROWS_PER_WORKER,
            "fixture_single_insert_per_worker": true,
            "native_cost_qualified": false,
        })
    );
    captures.verify_retained(&original).await?;
    Ok(evidence)
}

/// This is a finite synchronization assertion, not a substitute for a real native holder positive.
async fn capture_gated_original(captures: &mut OwnedCaptures) -> Result<(Captured, u32)> {
    let cancellation = CollectionCancellation::default();
    let read = async {
        let mut queries = 0;
        for _ in 0..MAX_CAPTURE_ATTEMPTS {
            if cancellation.is_cancelled() {
                return Err(CollectionStop::Cancelled.error());
            }
            let original = captures.capture_with_cancellation(CollectionSelection::Activity, &cancellation).await?;
            require_readable_capture(&original)?;
            queries += original.manifest.source_queries;
            if contains_all_gated_inserts(&original)? {
                return Ok((original, queries));
            }
        }
        Err(invalid("owned seed gate did not retain every original INSERT within its finite capture attempts"))
    };
    settle_original_before_deadline(&cancellation, read).await
}

/// Signal the existing capture owner at the observation deadline, then await its bounded native shutdown and
/// artifact finalization. No timeout drops the source, lease or close future, and unconfirmed debt stays intact.
async fn settle_original_before_deadline<T>(
    cancellation: &CollectionCancellation,
    read: impl Future<Output = Result<T>>,
) -> Result<T> {
    let deadline = tokio::time::Instant::now() + ORIGINAL_GATE_TIMEOUT;
    let (completed, done) = tokio::sync::oneshot::channel();
    let owner = async {
        let result = read.await;
        let _ = completed.send(());
        result
    };
    let relay = async {
        tokio::select! {
            biased;
            _ = done => {}
            _ = tokio::time::sleep_until(deadline) => cancellation.cancel(),
        }
    };
    let (result, ()) = tokio::join!(owner, relay);
    if tokio::time::Instant::now() >= deadline || cancellation.is_cancelled() {
        cancellation.cancel();
        result.and_then(|_| Err(CollectionStop::TimedOut.error()))
    } else {
        result
    }
}

fn contains_all_gated_inserts(original: &Captured) -> Result<bool> {
    let context = original.activity_context()?;
    if !context.has_qualified_local_owner() || context.database_writable.is_none() {
        return Err(invalid("owned original Activity did not qualify its actual member/process/role"));
    }
    let mut counts = [0; WRITER_APPLICATIONS.len()];
    let mut identities = BTreeSet::new();
    for row in &original.records {
        let TelemetryRecord::Activity(row) = &row.record else {
            return Err(invalid("owned original Activity included another fact family"));
        };
        let Some(index) = row
            .client
            .as_ref()
            .and_then(|client| client.application.as_deref())
            .and_then(|application| WRITER_APPLICATIONS.iter().position(|owned| *owned == application))
        else {
            continue;
        };
        if row.key.request.is_none()
            || row.scope != ActivityScopeBasis::RequestDatabase
            || row.command.as_deref() != Some("INSERT")
            || row.statement_identity.is_none()
            || row.waiting_on_lock != Some(true)
            || !row
                .wait
                .as_ref()
                .and_then(|wait| wait.native_type.as_deref())
                .is_some_and(|wait| wait.starts_with("LCK_M_"))
        {
            return Ok(false);
        }
        if !identities.insert(row.key.clone()) {
            return Err(invalid("owned original INSERT had ambiguous native request generations"));
        }
        counts[index] += 1;
    }
    Ok(counts == [WRITERS_PER_PATTERN; WRITER_APPLICATIONS.len()])
}

async fn inspect_holder(
    captures: &mut OwnedCaptures,
    focus: &PageTaskHolderFocus,
    attempt: usize,
    outcomes: &mut BTreeMap<String, u32>,
) -> Result<(usize, u32)> {
    let captured = captures.capture(CollectionSelection::PageTaskHolders { focus: Box::new(focus.clone()) }).await?;
    require_readable_capture(&captured)?;
    let source = captured.source()?;
    let context = NativeReadContext::from_snapshot(source)?;
    let [row] = captured.records.as_slice() else {
        return Err(invalid("native holder capture did not retain one aggregate"));
    };
    let TelemetryRecord::Operational(row) = &row.record else {
        return Err(invalid("native holder capture changed stream"));
    };
    let OperationalFact::PageTaskHolders(current) = &row.fact else {
        return Err(invalid("native holder capture changed fact"));
    };
    if !focus.accepts_observation(&context, current) {
        return Err(invalid("native holder result lost original source/task identity"));
    }
    for point in [&current.before, &current.after] {
        *outcomes.entry(format!("{:?}", point.holder.relation)).or_insert(0) += 1;
    }
    let qualified = validate_actual_points(current, &context)?;
    eprintln!(
        "{}",
        serde_json::json!({
            "phase":"owned-selected-native-task-holder","attempt":attempt,"qualified_points":qualified,
            "target":current.target_state,"before":current.before.holder.relation,"after":current.after.holder.relation,
            "native_queries":captured.manifest.source_queries,"capture_export_client_ms":captured.capture_export_ms,
            "original_activity_receipt":focus.participant.parent.key()?,"original_page_receipt":focus.page.key()?,
            "holder_durable_capture":captured.manifest.capture,"native_cost_qualified":false,
            "continuous_owner_proven":false,"contention_cause_proven":false,"read_metrics":captured.manifest.read_metrics,
        })
    );
    Ok((qualified, captured.manifest.source_queries))
}

fn validate_actual_points(row: &PageTaskHolderObservation, context: &NativeReadContext) -> Result<usize> {
    let points = [
        (row.page.as_ref().and_then(|page| page.task_wait.as_ref()), &row.before),
        (row.after_task.as_ref(), &row.after),
    ];
    let mut qualified = 0;
    for (task, point) in points {
        if point.holder.relation != PageTaskHolderRelation::QualifiedRequest {
            continue;
        }
        let task = task.ok_or_else(|| invalid("native holder positive omitted original waiting-task row"))?;
        let owner = point.holder.task.as_ref().ok_or_else(|| invalid("native holder task absent"))?;
        let request = point.holder.request.as_ref().ok_or_else(|| invalid("native holder request absent"))?;
        if !owner.matches_reported_holder(task)
            || !owner.matches_request(request)
            || request.key.request.is_none()
            || request.statement_identity.is_none()
            || owner.task_address == task.task_address
            || point.database_writable != context.database_writable
        {
            return Err(invalid("native holder proof did not bind the actual reported task and request generations"));
        }
        qualified += 1;
    }
    Ok(qualified)
}

fn captured_page(capture: &Captured, ordinal: usize) -> Result<&PageWaitObservation> {
    let Some(raw) = capture.records.get(ordinal) else {
        return Err(invalid("native task capture omitted its selected page row"));
    };
    let TelemetryRecord::Operational(raw) = &raw.record else {
        return Err(invalid("native task capture changed stream"));
    };
    let OperationalFact::PageWait(page) = &raw.fact else {
        return Err(invalid("native task capture changed fact"));
    };
    Ok(page)
}

fn original_focus(
    original: &Captured,
    pages: &Captured,
    ordinal: usize,
    page: &PageWaitObservation,
    outcomes: &mut BTreeMap<String, u32>,
) -> Result<Option<PageTaskHolderFocus>> {
    *outcomes.entry(format!("page-resolution:{:?}", page.resolution)).or_default() += 1;
    *outcomes.entry(format!("mapped-page-kind:{:?}", page.target.as_ref().map(|target| target.kind))).or_default() += 1;
    let Some(task) = &page.task_wait else {
        *outcomes.entry("coordinator-only-page".into()).or_default() += 1;
        return Ok(None);
    };
    *outcomes.entry(format!("native-task-wait:{}", task.wait_type)).or_default() += 1;
    if page.resolution == PageWaitResolution::UnrecognizedAddress {
        *outcomes.entry("unrecognized-task-resource".into()).or_default() += 1;
    }
    if !task.blocking_session_id.is_some_and(|id| id > 0) {
        *outcomes.entry(format!("native-holder-session:{:?}", task.blocking_session_id)).or_default() += 1;
    }
    if task.blocking_task_address.as_ref().is_none_or(|address| address.bytes().all(|byte| byte == b'0')) {
        *outcomes.entry("native-holder-address-unavailable".into()).or_default() += 1;
    }
    original.with_activity_frame(|frame| {
        match plan_page_task_holder_focus(pages.source()?, page, &pages.reference(ordinal)?, frame)? {
            DiagnosticFocusPlan::Ready(focus) => match *focus {
                DiagnosticFocus::PageTaskHolders { focus } => Ok(Some(*focus)),
                _ => Err(invalid("native page planner selected a different question")),
            },
            DiagnosticFocusPlan::NotRequired => {
                *outcomes.entry("not-required".into()).or_default() += 1;
                Ok(None)
            }
            DiagnosticFocusPlan::Unavailable(issue) => {
                *outcomes.entry(format!("unavailable:{issue:?}")).or_default() += 1;
                identity_receipt::note(outcomes, frame, pages.source()?, page, issue);
                Ok(None)
            }
        }
    })
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn original_gate_deadline_signals_then_awaits_the_same_capture_owner_and_preserves_its_shutdown_result() {
        for stop in [CollectionStop::Cancelled, CollectionStop::CancellationUnconfirmed] {
            let cancellation = CollectionCancellation::default();
            let stop_token = cancellation.clone();
            let (started, running) = tokio::sync::oneshot::channel();
            let (closing, close_started) = tokio::sync::oneshot::channel();
            let (release, settle) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let owner = async {
                    started.send(()).unwrap();
                    cancellation.cancelled().await;
                    closing.send(()).unwrap();
                    settle.await.unwrap();
                    Err::<(), _>(stop.error())
                };
                settle_original_before_deadline(&cancellation, owner).await
            });
            running.await.unwrap();
            tokio::time::advance(ORIGINAL_GATE_TIMEOUT).await;
            close_started.await.unwrap();
            assert!(stop_token.is_cancelled());
            assert!(!task.is_finished(), "the deadline cannot detach the capture's native shutdown or artifact owner");
            release.send(()).unwrap();
            let error = task.await.unwrap().unwrap_err();
            assert_eq!(CollectionStop::from_error(&error), stop);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_settled_original_capture_releases_the_gate_without_waiting_for_or_cancelling_its_deadline() {
        let cancellation = CollectionCancellation::default();
        let started = tokio::time::Instant::now();
        let value = settle_original_before_deadline(&cancellation, async { Ok(7) }).await.unwrap();
        assert_eq!(value, 7);
        assert!(!cancellation.is_cancelled());
        assert!(started.elapsed() < ORIGINAL_GATE_TIMEOUT);
    }
}
