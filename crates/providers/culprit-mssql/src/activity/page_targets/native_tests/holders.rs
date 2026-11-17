//! Native qualification, not a fabricated latch owner or cost promotion. Some engines/builds expose only
//! sentinel/unknown latch owners; a finite fixture that cannot qualify a holder reports that exact gap.
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

const MAX_CAPTURE_ATTEMPTS: usize = 8;
const MAX_IDENTITY_DIAGNOSTIC_SAMPLES: u32 = 4;
const ORIGINAL_GATE_TIMEOUT: Duration = Duration::from_secs(3);

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn an_owned_native_page_task_holder_requires_the_reported_pointer_and_exact_current_request() {
    fixture::run(|fixture| Box::pin(qualify_owned_holder(fixture))).await;
}

async fn qualify_owned_holder(fixture: &mut Fixture) -> Result<()> {
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
    let mut qualified = false;
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
        let mut selected = None;
        for ordinal in 0..pages.records.len() {
            if let Some(focus) = original_focus(&original, &pages, ordinal, &mut planning_outcomes)? {
                selected = Some(focus);
                break;
            }
        }
        let Some(focus) = selected else { continue };
        captures.verify_retained(&original).await?;
        captures.verify_retained(&pages).await?;
        let (points, queries) = inspect_holder(&mut captures, &focus, attempt, &mut outcomes).await?;
        captures.verify_retained(&original).await?;
        captures.verify_retained(&pages).await?;
        query_count += queries;
        if points > 0 {
            qualified = true;
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
            "native_holder_relations": outcomes, "native_positive": qualified,
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
    if !qualified {
        return Err(invalid(
            "finite owned native page workload did not expose a qualified blocking-task/request relationship; no positive proof",
        ));
    }
    Ok(())
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

fn original_focus(
    original: &Captured,
    pages: &Captured,
    ordinal: usize,
    outcomes: &mut BTreeMap<String, u32>,
) -> Result<Option<PageTaskHolderFocus>> {
    let Some(raw) = pages.records.get(ordinal) else { return Ok(None) };
    let TelemetryRecord::Operational(raw) = &raw.record else { return Ok(None) };
    let OperationalFact::PageWait(page) = &raw.fact else { return Ok(None) };
    *outcomes.entry(format!("page-resolution:{:?}", page.resolution)).or_default() += 1;
    *outcomes.entry(format!("mapped-page-kind:{:?}", page.target.as_ref().map(|target| target.kind))).or_default() += 1;
    let Some(task) = &page.task_wait else {
        *outcomes.entry("coordinator-only-page".into()).or_default() += 1;
        return Ok(None);
    };
    *outcomes.entry(format!("native-task-wait:{}", task.wait_type)).or_default() += 1;
    if page.resolution == PageWaitResolution::UnrecognizedAddress {
        let seen = outcomes.entry("unrecognized-task-resource-samples".into()).or_default();
        if *seen < 4 {
            eprintln!("{}", serde_json::json!({"phase":"owned-native-task-resource","raw":task.resource.native.id}));
        }
        *seen += 1;
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
                if issue == DiagnosticAnalysisIssue::MissingNativeIdentity {
                    note_missing_identity(outcomes, frame, pages.source()?, page);
                }
                Ok(None)
            }
        }
    })
}

/// Diagnostic prerequisites only. The real planner above remains the sole authority and no SQL, payload,
/// native token, task pointer or request key is printed or used to repair its result.
fn note_missing_identity(
    outcomes: &mut BTreeMap<String, u32>,
    frame: &ActivityFrameEvidence<'_>,
    page_source: &CollectionDescription,
    page: &PageWaitObservation,
) {
    let original = frame.row(&page.request.key);
    let token = original.and_then(|row| row.observation.statement_identity.as_ref());
    let original_qualified = frame.context().has_qualified_local_owner();
    let page_qualified =
        page_source.is_partition_qualified() && matches!(page_source.partition, SourcePartition::InstanceLocal { .. });
    for (key, missing) in [
        ("original-scope", !original_qualified),
        ("current-page-scope", !page_qualified),
        ("original-request-key", original.is_none()),
        ("original-receipt", original.is_some_and(|row| row.reference.is_none())),
        ("original-statement-token", original.is_some_and(|row| row.observation.statement_identity.is_none())),
        ("original-epoch", frame.native().epoch.is_none()),
        ("current-page-statement-token", page.statement_identity.is_none()),
    ] {
        if missing {
            *outcomes.entry(format!("missing-identity:{key}")).or_default() += 1;
        }
    }
    let samples = outcomes.entry("missing-identity-diagnostic-samples".into()).or_default();
    if *samples >= MAX_IDENTITY_DIAGNOSTIC_SAMPLES {
        return;
    }
    *samples += 1;
    let payload_reason = match &page.request.statement {
        DefinitionPayload::Unavailable { reason } => Some(*reason),
        _ => None,
    };
    eprintln!(
        "{}",
        serde_json::json!({
            "phase":"owned-native-task-identity-gap",
            "original_scope_qualified":original_qualified,"page_scope_qualified":page_qualified,
            "original_request_key_present":original.is_some(),
            "original_receipt_present":original.is_some_and(|row|row.reference.is_some()),
            "original_statement_token_present":token.is_some(),"original_epoch_present":frame.native().epoch.is_some(),
            "raw_current_request_token_present":page.request.statement_identity.is_some(),
            "raw_current_request_token_matches_original":page.request.statement_identity.as_ref().zip(token).map(|(a,b)|a==b),
            "qualified_page_statement_token_present":page.statement_identity.is_some(),
            "qualified_page_statement_token_matches_original":page.statement_identity.as_ref().zip(token).map(|(a,b)|a==b),
            "statement_payload_available":page.request.statement.is_available(),"statement_payload_reason":payload_reason,
            "before_request":page.before_check.as_ref().map(|check|check.request),
            "before_wait":page.before_check.as_ref().map(|check|check.wait),
            "after_request":page.after_check.as_ref().map(|check|check.request),
            "after_wait":page.after_check.as_ref().map(|check|check.wait),
        })
    );
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
