//! One original scalar execution waits for a native grant while its same-input/work counterpart does not
//! have the held-output actor. The source, completed work and retained history remain independent evidence.
use super::*;
use crate::activity::memory::native_tests::{CONTROL_TIMEOUT, fixture::Fixture};
use culprit_core::{MemoryParticipantMechanism, SourceOutcome, plan_compare::NativeRuntimeInputRelation};
use futures_util::future::LocalBoxFuture;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

const ORIGINAL_SOURCE_QUERY_CAP: u32 = 9 + 9 + 12;

struct Episode {
    retained: Original,
    holder_point: Captured,
    waiting: Captured,
    original_waiter: ExecutionGrantObservation,
    holder: ExecutionGrantObservation,
    original: Box<RecordedRun>,
    counterpart: Box<RecordedRun>,
    original_controller_window: Duration,
}

pub(crate) async fn prove(fixture: &mut Fixture) -> culprit_core::Result<()> {
    let cancellation = fixture.cancellation.clone();
    let mut probe = start_scalar_probe(fixture).await?;
    let actor = probe.actor();
    let outcome = AssertUnwindSafe(capture_episode(fixture, &mut probe)).catch_unwind().await;
    // This phase owns the original scalar client, including timeout/panic paths in the existing capture owner.
    // A successful physical close never repairs a failed/unconfirmed original execution.
    let source_shutdown = probe.pool.source_shutdown();
    let closed = crate::test_support::owned_case::retire(probe.application).await;
    let mut episode = match outcome {
        Ok(result) => result?,
        Err(original) => std::panic::resume_unwind(original),
    };
    closed?;
    crate::test_support::owned_case::require_active(&cancellation)?;
    if source_shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let (admitted, database) = fixture.original_context()?;
    // Pin live and closed matching through the existing exact-generation verifier, never a bare session number.
    episode.retained.verify_after_actor_disappears(admitted, database, actor.clone()).await;
    episode.retained.owner.verify_retained(&episode.holder_point).await?;
    episode.retained.owner.verify_retained(&episode.waiting).await?;
    let report = grant_report(&episode.waiting)?;
    require_waiting_episode(&episode.waiting, &actor, &episode.original.context, &episode.holder)?;
    assert_eq!(
        crate::plan_xml::runtime_input_pair(&episode.retained.explain(), &episode.counterpart.plan).relation(),
        NativeRuntimeInputRelation::Unavailable,
        "retained payloads cannot borrow ephemeral runtime values from the executed comparison"
    );
    let captures = [&episode.holder_point, &episode.waiting, &episode.retained.capture];
    let source_queries = captures.iter().map(|capture| capture.manifest.source_queries).sum::<u32>();
    assert!(source_queries <= ORIGINAL_SOURCE_QUERY_CAP);
    crate::test_support::save_cost_receipt(
        &fixture.directory,
        "original-scalar-grant-pressure",
        &json!({
            "successful":true,
            "captures":captures.iter().map(|capture| &capture.manifest).collect::<Vec<_>>(),
            "original_holder_point_records":episode.holder_point.records,
            "original_memory_records":episode.waiting.records,
            "original_retained_records":episode.retained.capture.records,
            "original_waiter":episode.original_waiter,
            "original_holder":episode.holder,
            "original_memory_report":report,
            "original_plan":culprit_core::redact::redact_plan(&episode.original.plan),
            "counterpart_plan":culprit_core::redact::redact_plan(&episode.counterpart.plan),
            "same_complete_runtime_input_tuple":true,
            "same_cached_statement_incarnation":true,
            "same_result_and_own_access_work":true,
            "native_admission_delay_decreased":true,
            "native_original_grant_wait_ms":episode.original.plan.facts().memory_grant_wait_ms,
            "native_counterpart_grant_wait_ms":episode.counterpart.plan.facts().memory_grant_wait_ms,
            "retained_source_grant_wait_ms":episode.retained.explain().facts().memory_grant_wait_ms,
            "original_controller_window_ms":episode.original_controller_window.as_secs_f64()*1000.0,
            "controller_window_includes_metadata_and_is_only_an_elapsed_upper_bound":true,
            "source_queries":source_queries,
            "canonical_source_dispatch_cap":ORIGINAL_SOURCE_QUERY_CAP,
            "fixture_holder_observation_limit":1,
            "canonical_source_captures":3,
            "holder_interventions":1,
            "separately_paid_scalar_fixture_dispatch_caps": {
                "probe_preparation_including_own_session_setup":9,
                "same_client_native_resource_route":1,
                "three_original_actual_capture_commands_each_three_dispatches":9,
                "cache_metadata_and_bindings":10,
                "metadata_reader_session_setups":3,
                "two_module_selections_with_conditional_same_caller_exact_checks":6,
                "pre_execution_conditional_predicate_diagnostic_dispatch_cap":2,
                "post_execution_conditional_module_diagnostic_dispatch_cap":2,
                "cache_transition_reader_session_setups":2,
                "live_and_closed_original_actor_verifiers_including_session_setups":6,
                "total":50,
            },
            "existing_grant_fixture_work_separate_from_canonical_sources":true,
            "held_holder_point_preparation_and_observer_work":"none; all original source reads use the same canonical capture owner",
            "source_session_setup_is_separate_from_manifest_source_queries":true,
            "changed_sql_input_plan_statistics_or_forcing":false,
            "elapsed_difference_attributed_to_parameter_or_grant":false,
            "native_cost_approved":false,
        }),
    )?;
    crate::test_support::owned_case::require_active(&cancellation)?;
    fixture.control_completed();
    Ok(())
}

fn capture_episode<'a>(
    fixture: &'a mut Fixture,
    probe: &'a mut Probe,
) -> LocalBoxFuture<'a, culprit_core::Result<Box<Episode>>> {
    Box::pin(async move {
        let cancellation = fixture.cancellation.clone();
        crate::test_support::owned_case::require_active(&cancellation)?;
        let actor = probe.actor();
        let original_actor = fixture.verify_scalar_route(&mut probe.application).await?;
        assert_eq!(original_actor, native_actor(&actor));
        let prime = probe.record(2, CacheExpectation::New).await;
        assert_context(&prime, ParameterValues::Same);
        // The pre-existing resource profile, finite sort and original stream are reused without a second actor path.
        fixture.start_actor().await?;
        fixture.pause().await?;
        crate::test_support::owned_case::require_active(&cancellation)?;
        probe
            .observe_original_cache_context(
                &prime.context,
                &fixture.directory,
                "cache-after-holder-before-memory-source",
            )
            .await?;
        let mut owner = OwnedCaptures::from_pool(probe.pool.clone(), probe.port)?;
        let holder_point = owner
            .capture_with_cancellation(
                CollectionSelection::Snapshot { dataset: SnapshotDataset::CurrentRequestMemory },
                &cancellation,
            )
            .await?;
        // Retain the successful canonical source before a later cache guard can fail and drop this fixture's
        // temporary Outbox. These are the actual sealed original rows and accounting, never a recreated snapshot.
        crate::test_support::save_cost_receipt(
            &fixture.directory,
            "canonical-memory-source-before-cache-recheck",
            &json!({
                "phase":"original-memory-capture-complete-before-cache-recheck",
                "manifest":holder_point.manifest,
                "original_records":holder_point.records,
                "capture_export_ms":holder_point.capture_export_ms,
                "cache_transition_cause_established":false,
                "native_cost_approved":false,
            }),
        )?;
        let holder = original_holder(&holder_point, fixture.session(0)?)?.clone();
        probe
            .observe_current_cache_context(
                &prime.context,
                &fixture.directory,
                "cache-after-memory-source-before-original",
            )
            .await?;
        assert!(fixture.outputs_are_held());
        fixture.record_observed_request_demand(fixture.session(0)?.clone(), &holder)?;
        fixture.require_next_demand()?;
        let holder_source = holder_point.source()?.clone();
        let submitting = Notify::new();
        let diagnostic_directory = fixture.directory.clone();
        let (executed, captured) = tokio::join!(
            AssertUnwindSafe(async {
                let started = Instant::now();
                let original = probe
                    .record_native_notifying(
                        1,
                        CacheExpectation::Reused(&prime.context),
                        &submitting,
                        Some(&diagnostic_directory),
                    )
                    .await;
                (original, started.elapsed())
            })
            .catch_unwind()
            .inspect(|outcome| crate::test_support::owned_case::cancel_after_panic(outcome, &cancellation)),
            capture_waiter_and_release(
                fixture,
                &mut owner,
                &submitting,
                &actor,
                &prime.context,
                &holder,
                &holder_source,
            ),
        );
        // Both borrowed owners settle before a failure is propagated. A source failure never causes another capture.
        let (original, original_controller_window) = match executed {
            Ok(run) => run,
            Err(original) => std::panic::resume_unwind(original),
        };
        let observed_waiter = match captured {
            Ok(result) => result?,
            Err(original) => std::panic::resume_unwind(original),
        };
        let waiting = &observed_waiter.0;
        assert_context(&original, ParameterValues::Different);
        crate::test_support::owned_case::require_active(&cancellation)?;
        let retained_capture = owner
            .capture_with_cancellation(
                CollectionSelection::Snapshot { dataset: SnapshotDataset::RetainedPlans },
                &cancellation,
            )
            .await?;
        let mut retained = Original::from_capture(owner, retained_capture, &original, cancellation.clone());
        let (admitted, database) = fixture.original_context()?;
        verify_actor_count(admitted, database, &actor, 1, &cancellation).await;
        assert_eq!(waiting.source()?.target, retained.capture.source()?.target);
        assert_eq!(waiting.source()?.partition, retained.capture.source()?.partition);
        let earlier = waiting.source()?.observation.snapshot().unwrap();
        let later = retained.capture.source()?.observation.snapshot().unwrap();
        assert!(ObservationBounds::new(&earlier.observed_until, &later.observed_from).is_some());
        // No sp_recompile, statistics update, forcing, input change or replacement request: holder drain alone changed.
        crate::test_support::owned_case::require_active(&cancellation)?;
        let counterpart = probe.record_native(1, CacheExpectation::Reused(&original.context)).await;
        crate::test_support::save_cost_receipt(
            &fixture.directory,
            "completed-original-and-counterpart",
            &json!({
                "phase":"both-original-responses-complete-before-comparison",
                "retained_manifest":retained.capture.manifest,
                "original_retained_records":retained.capture.records,
                "original_plan":culprit_core::redact::redact_plan(&original.plan),
                "counterpart_plan":culprit_core::redact::redact_plan(&counterpart.plan),
                "original_controller_window_ms":original_controller_window.as_secs_f64()*1000.0,
                "original_sampled_wait_ms":observed_waiter.1.wait_ms,
                "controller_window_includes_metadata_and_is_only_an_elapsed_upper_bound":true,
                "native_cost_approved":false,
            }),
        )?;
        require_native_wait_window(&original, &observed_waiter.1, original_controller_window);
        require_unchanged_execution(&original, &counterpart);
        retained.owner.verify_retained(waiting).await?;
        let (waiting, original_waiter) = *observed_waiter;
        Ok(Box::new(Episode {
            retained,
            holder_point,
            waiting,
            original_waiter,
            holder,
            original,
            counterpart,
            original_controller_window,
        }))
    })
}

type WaiterOutcome = std::thread::Result<culprit_core::Result<Box<(Captured, ExecutionGrantObservation)>>>;

fn capture_waiter_and_release<'a>(
    fixture: &'a mut Fixture,
    owner: &'a mut OwnedCaptures,
    submitting: &'a Notify,
    actor: &'a probe::Actor,
    statement: &'a CachedStatementContext,
    holder: &'a ExecutionGrantObservation,
    holder_source: &'a CollectionDescription,
) -> LocalBoxFuture<'a, WaiterOutcome> {
    Box::pin(async move {
        // One unwind owner while the original native capture is active. Return its completed outcome instead
        // of rethrowing it into a second caller-side unwind poll frame.
        let captured = AssertUnwindSafe(Box::pin(capture_waiter(
            fixture,
            owner,
            submitting,
            actor,
            statement,
            holder,
            holder_source,
        )))
        .catch_unwind()
        .await;
        // Always attempt cleanup of the SAME held stream, after capture settlement. Cleanup's separate unwind
        // boundary is sequential; neither its panic nor its error replaces the original capture failure.
        let drained = AssertUnwindSafe(Box::pin(fixture.drain_original_holder()))
            .catch_unwind()
            .await
            .map(|result| result.map(|_| ()));
        after_holder_cleanup(captured, drained)
    })
}

async fn capture_waiter(
    fixture: &mut Fixture,
    owner: &mut OwnedCaptures,
    submitting: &Notify,
    actor: &probe::Actor,
    statement: &CachedStatementContext,
    holder: &ExecutionGrantObservation,
    holder_source: &CollectionDescription,
) -> culprit_core::Result<Box<(Captured, ExecutionGrantObservation)>> {
    let cancellation = fixture.cancellation.clone();
    crate::test_support::owned_case::until_cancelled(&cancellation, async {
        tokio::time::timeout(CONTROL_TIMEOUT, submitting.notified()).await.map_err(|_| CollectionStop::TimedOut.error())
    })
    .await?;
    let captured = Box::pin(owner.capture_with_cancellation(
        CollectionSelection::Snapshot { dataset: SnapshotDataset::CurrentRequestMemory },
        &cancellation,
    ))
    .await?;
    assert_eq!(captured.source()?.target, holder_source.target);
    assert_eq!(captured.source()?.partition, holder_source.partition);
    let before = holder_source.observation.snapshot().unwrap();
    let after = captured.source()?.observation.snapshot().unwrap();
    assert!(ObservationBounds::new(&before.observed_until, &after.observed_from).is_some());
    let waiter = require_waiting_episode(&captured, actor, statement, holder)?.clone();
    let mut reused_session = actor.clone();
    reused_session.generation = "2000-01-01T00:00:00.0000000".into();
    assert!(
        require_waiting_episode(&captured, &reused_session, statement, holder).is_err(),
        "the same numeric session cannot borrow the original request after its login generation changes"
    );
    let request = waiter.request.as_ref().unwrap();
    let mut another_statement = statement.clone();
    another_statement.identity.sql_handle = "0x01020304".into();
    assert!(!matches_native_statement(request, &another_statement));
    fixture.record_observed_request_demand(native_actor(actor), &waiter)?;
    crate::test_support::save_cost_receipt(
        &fixture.directory,
        "original-memory-before-holder-drain",
        &json!({
            "phase":"original-waiter-observed-before-intervention",
            "manifest":captured.manifest,
            "original_records":captured.records,
            "same_original_holder":holder,
            "original_scalar_waiter":waiter,
            "intervention_performed":false,
            "native_cost_approved":false,
        }),
    )?;
    Ok(Box::new((captured, waiter)))
}

fn after_holder_cleanup<T>(
    captured: std::thread::Result<culprit_core::Result<T>>,
    drained: std::thread::Result<culprit_core::Result<()>>,
) -> std::thread::Result<culprit_core::Result<T>> {
    match captured {
        Err(original) => Err(original),
        Ok(Err(original)) => Ok(Err(original)),
        Ok(Ok(captured)) => match drained {
            Err(original) => Err(original),
            Ok(Err(error)) => Ok(Err(error)),
            Ok(Ok(())) => Ok(Ok(captured)),
        },
    }
}

#[test]
fn original_capture_failure_remains_primary_after_secondary_holder_cleanup_failure() {
    use culprit_core::{Error, ErrorCode};
    let native = Error::new(ErrorCode::QueryFailed, "original native capture failed");
    let cleanup = Error::new(ErrorCode::PermissionDenied, "secondary cleanup failed");
    let observed = after_holder_cleanup::<()>(Ok(Err(native)), Ok(Err(cleanup))).unwrap().unwrap_err();
    assert_eq!(observed.code, ErrorCode::QueryFailed);
    let observed =
        after_holder_cleanup::<()>(Err(Box::new("original panic")), Err(Box::new("cleanup panic"))).unwrap_err();
    assert_eq!(observed.downcast_ref::<&str>(), Some(&"original panic"));
    let observed =
        after_holder_cleanup::<()>(Ok(Err(CollectionStop::SourceUnavailable.error())), Err(Box::new("cleanup panic")))
            .unwrap()
            .unwrap_err();
    assert_eq!(CollectionStop::from_error(&observed), CollectionStop::SourceUnavailable);
    let observed = after_holder_cleanup(Ok(Ok(17)), Ok(Err(CollectionStop::CancellationUnconfirmed.error())))
        .unwrap()
        .unwrap_err();
    assert_eq!(CollectionStop::from_error(&observed), CollectionStop::CancellationUnconfirmed);
    assert_eq!(after_holder_cleanup(Ok(Ok(17)), Ok(Ok(()))).unwrap().unwrap(), 17);
}

fn native_actor(actor: &probe::Actor) -> NativeActivityIdentity {
    NativeActivityIdentity {
        id: actor.id.to_string(),
        generation: crate::sql_time::native_time(&actor.generation).unwrap(),
    }
}

fn grants(captured: &Captured) -> Vec<&ExecutionGrantObservation> {
    captured
        .records
        .iter()
        .map(|row| {
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::ExecutionGrant(grant),
                ..
            }) = &row.record
            else {
                panic!("original execution grants only")
            };
            grant.as_ref()
        })
        .collect()
}

fn grant_report(captured: &Captured) -> culprit_core::Result<culprit_core::MemoryParticipantsReport> {
    let source = captured.source()?;
    let context = NativeReadContext::from_snapshot(source)?;
    let coverage = &source.observation.snapshot().ok_or_else(crate::read::invalid_data)?.sources[0];
    let outcome =
        SourceOutcome { source: coverage.source.clone(), state: coverage.state, reason: None, reason_message: None };
    culprit_checks::request_memory::explain(
        culprit_checks::request_memory::RequestMemoryEvidence { context: &context, observations: &grants(captured) },
        Some(&outcome),
    )
}

fn original_holder<'a>(
    captured: &'a Captured,
    session: &NativeActivityIdentity,
) -> culprit_core::Result<&'a ExecutionGrantObservation> {
    let report = grant_report(captured)?;
    let rows = grants(captured);
    let candidates = rows
        .iter()
        .copied()
        .filter(|row| row.request.as_ref().is_some_and(|request| &request.key.session == session))
        .collect::<Vec<_>>();
    let [holder] = candidates.as_slice() else { return Err(crate::read::invalid_data()) };
    assert!(report.entries.iter().any(|entry| entry.observation_identity == holder.identity
        && entry.mechanism == Some(MemoryParticipantMechanism::ReservationDuringClientOutputWait)));
    Ok(*holder)
}

fn require_waiting_episode<'a>(
    capture: &'a Captured,
    actor: &probe::Actor,
    statement: &CachedStatementContext,
    original_holder: &ExecutionGrantObservation,
) -> culprit_core::Result<&'a ExecutionGrantObservation> {
    let report = grant_report(capture)?;
    let actor = native_actor(actor);
    let rows = grants(capture);
    let waiting = rows
        .iter()
        .copied()
        .filter(|grant| grant.request.as_ref().is_some_and(|request| request.key.session == actor))
        .collect::<Vec<_>>();
    let [waiter] = waiting.as_slice() else { return Err(crate::read::invalid_data()) };
    let request = waiter.request.as_ref().unwrap();
    assert!(request.key.request.is_some());
    assert_eq!(waiter.request_association, RequestGrantAssociation::GenerationChecked);
    assert_eq!(waiter.statement_relationship, GrantStatementRelationship::ObservedCurrentStatement);
    assert_eq!(waiter.state, ExecutionGrantState::Waiting);
    assert!(waiter.wait_ms.as_deref().is_some_and(|value| value.parse::<u64>().is_ok_and(|value| value > 0)));
    assert!(matches!(waiter.granted_kibibytes.as_deref(), None | Some("0")));
    assert!(matches_native_statement(request, statement));
    let held = rows.iter().copied().filter(|row| row.identity == original_holder.identity).collect::<Vec<_>>();
    let [held] = held.as_slice() else { return Err(crate::read::invalid_data()) };
    assert_eq!(held.request.as_ref().unwrap().key, original_holder.request.as_ref().unwrap().key);
    assert_eq!(held.resource, original_holder.resource);
    assert!(held.shares_admission_resource(waiter, &capture.source()?.target.database().unwrap().database));
    assert!(report.entries.iter().any(|entry| entry.observation_identity == held.identity
        && entry.mechanism == Some(MemoryParticipantMechanism::ReservationDuringClientOutputWait)));
    assert!(report.entries.iter().any(|entry| entry.observation_identity == waiter.identity
        && entry.qualified_request.as_ref() == Some(&request.key)
        && entry.grant_admission == waiter.resource));
    Ok(*waiter)
}

fn require_native_wait_window(original: &RecordedRun, waiter: &ExecutionGrantObservation, window: Duration) {
    let native_wait_ms = original.plan.facts().memory_grant_wait_ms.expect("the completed native grant wait");
    let sampled_wait_ms = waiter
        .wait_ms
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .expect("positive native wait already observed for the same original request");
    assert!(
        native_wait_fits_controller_window(native_wait_ms, sampled_wait_ms, window),
        "native grant wait {native_wait_ms}ms must cover the sampled {sampled_wait_ms}ms and fit the same controller's {window:?} window"
    );
}

fn native_wait_fits_controller_window(native_wait_ms: f64, sampled_wait_ms: u64, window: Duration) -> bool {
    // This owner includes metadata, transport and response draining. Its duration is only an elapsed upper
    // bound, never native execution/CPU time, peak cost, or exclusive attribution to a grant holder.
    sampled_wait_ms > 0
        && native_wait_ms.is_finite()
        && native_wait_ms >= sampled_wait_ms as f64
        && native_wait_ms <= window.as_secs_f64() * 1000.0
}

#[test]
fn native_grant_wait_milliseconds_fit_only_the_same_request_observed_window() {
    let window = Duration::from_millis(4072);
    assert!(native_wait_fits_controller_window(320.0, 43, window));
    assert!(!native_wait_fits_controller_window(320_000.0, 43, window), "seconds conversion inflates native wait");
    assert!(!native_wait_fits_controller_window(0.320, 43, window), "microseconds cannot cover the observed wait");
    assert!(!native_wait_fits_controller_window(320.0, 321, window), "the sampled wait is a lower bound");
    assert!(!native_wait_fits_controller_window(320.0, 0, window), "an unknown/zero sample proves no positive wait");
    assert!(!native_wait_fits_controller_window(320.0, 43, Duration::from_millis(319)));
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        assert!(!native_wait_fits_controller_window(invalid, 43, window));
    }
}

fn require_unchanged_execution(original: &RecordedRun, counterpart: &RecordedRun) {
    assert_eq!(original.context.identity.key().unwrap(), counterpart.context.identity.key().unwrap());
    assert_eq!(original.result, counterpart.result);
    let pair = crate::plan_xml::runtime_input_pair(&original.plan, &counterpart.plan);
    assert_eq!(pair.relation(), NativeRuntimeInputRelation::Same);
    for physical in ["Index Seek", "Key Lookup"] {
        let before = original.operator(physical);
        let after = counterpart.operator(physical);
        assert_eq!(before.object, after.object);
        assert_eq!(before.facts().table_alias, after.facts().table_alias);
        assert_eq!(before.act_rows, after.act_rows);
        assert_eq!(before.act_executions, after.act_executions);
        assert!(before.act_executions.is_some_and(|count| count.is_finite() && count > 0.0));
        assert_eq!(before.facts().parallel_operator, Some(false));
        assert_eq!(after.facts().parallel_operator, Some(false));
        assert_eq!(original.logical_reads(physical), counterpart.logical_reads(physical));
    }
    assert!(original.logical_reads("Key Lookup") >= 1000.0);
    let before_wait = original
        .plan
        .facts()
        .memory_grant_wait_ms
        .filter(|value| value.is_finite() && *value > 0.0)
        .expect("positive native original grant-admission delay");
    let after_wait = counterpart
        .plan
        .facts()
        .memory_grant_wait_ms
        .filter(|value| value.is_finite() && *value >= 0.0)
        .expect("the counterpart must record its own grant delay; unavailable is never zero");
    assert!(
        after_wait < before_wait,
        "the owned holder-drain control must actually reduce native admission delay while access work stays fixed"
    );
    // The native admission-delay comparison is separate from total elapsed time or an exclusive cause.
    for plan in [&original.plan, &counterpart.plan] {
        let insights = culprit_core::insights::insights(plan);
        let contexts = insights.iter().filter(|insight| insight.rule == "parameter-context").collect::<Vec<_>>();
        let [insight] = contexts.as_slice() else { panic!("one real native parameter-context explanation") };
        assert_eq!(insight.severity, Severity::Low);
        assert_eq!(insight.impact, 0.0);
        assert_eq!(insight.fix, culprit_core::t!("insight.parameter_context.fix"));
        assert!(insight.fix_sql.is_none());
    }
    let compared =
        culprit_core::plan_compare::compare_native_pair(pair, "Original queue", "Holder released", &Default::default());
    assert!(
        compared.scenarios.iter().all(|scenario| scenario.kind != culprit_core::ScenarioKind::Parameters),
        "equal native inputs with the same lookup work must not invent a lookup-to-scan/reuse intervention"
    );
}

async fn start_scalar_probe(fixture: &mut Fixture) -> culprit_core::Result<Box<Probe>> {
    let cancellation = fixture.cancellation.clone();
    crate::test_support::owned_case::require_active(&cancellation)?;
    fixture.begin_scalar_actor()?;
    let application = fixture.scalar_application()?;
    let (admitted, database) = fixture.original_context()?;
    Ok(Box::new(
        Probe::granted_counted(admitted, database, IndexCoverage::LookupRequired, &application, &cancellation).await,
    ))
}

/// Diagnostic only: either fully observed final selection outcome is useful. Neither closes G3 grant acceptance.
pub(crate) async fn cache_without_collection(fixture: &mut Fixture) -> culprit_core::Result<()> {
    let mut probe = start_scalar_probe(fixture).await?;
    let outcome = Box::pin(
        AssertUnwindSafe(async {
            let actor = probe.actor();
            assert_eq!(fixture.verify_scalar_route(&mut probe.application).await?, native_actor(&actor));
            let prime = probe.record(2, CacheExpectation::New).await;
            assert_context(&prime, ParameterValues::Same);
            fixture.start_actor().await?;
            fixture.pause().await?;
            assert!(fixture.outputs_are_held(), "the same original client stream remains held");
            let window = probe.observe_exact_cache_without_collection(&prime.context, &fixture.directory).await?;
            assert!(fixture.outputs_are_held(), "the original holder stream was not released or replaced");
            crate::test_support::save_cost_receipt(
                &fixture.directory,
                "no-memory-collection-control",
                &json!({
                    "diagnostic_completed":true,
                    "exact_original_window":window,
                    "original_cached_statement":prime.context,
                    "original_scalar_actor":native_actor(&actor),
                    "holder_stream_alive_before_and_after":true,
                    "holder_native_grant_sampled":false,
                    "canonical_memory_captures":0,"canonical_source_queries":0,
                    "replacement_scalar_executions":0,
                    "scalar_grant_acceptance_established":false,
                    "cache_removal_cause_established":false,
                    "native_cost_approved":false,
                    "paid_scalar_fixture_dispatch_caps":{
                        "preparation_including_own_session_setup":9,
                        "same_client_route_verification":1,
                        "original_prime_actual_capture_commands":3,
                        "prime_cache_metadata_and_binding":2,
                        "prime_metadata_reader_session_setup":1,
                        "exact_before_after_metadata_and_bindings":4,
                        "quiet_window_metadata_reader_session_setups":2,
                        "total":22,
                    },
                    "existing_grant_fixture_setup_actor_and_cleanup_separately_paid":true,
                }),
            )?;
            Ok::<_, culprit_core::Error>(())
        })
        .catch_unwind(),
    )
    .await;
    let shutdown = probe.pool.source_shutdown();
    let closed = crate::test_support::owned_case::retire(probe.application).await;
    let result = match outcome {
        Ok(result) => result,
        Err(original) => std::panic::resume_unwind(original),
    };
    result?;
    closed?;
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    crate::test_support::owned_case::require_active(&fixture.cancellation)?;
    fixture.control_completed();
    Ok(())
}

pub(crate) async fn source_phase_control(fixture: &mut Fixture) -> culprit_core::Result<()> {
    let mut probe = start_scalar_probe(fixture).await?;
    let outcome = Box::pin(
        AssertUnwindSafe(async {
            let actor = probe.actor();
            assert_eq!(fixture.verify_scalar_route(&mut probe.application).await?, native_actor(&actor));
            let prime = probe.record(2, CacheExpectation::New).await;
            assert_context(&prime, ParameterValues::Same);
            fixture.start_actor().await?;
            fixture.pause().await?;
            assert!(fixture.outputs_are_held());
            let (admitted, database) = fixture.original_context()?;
            let observer = Arc::new(Pool::new(
                config(admitted.port, database, admitted.password(), "culprit-cache-phase-observer"),
                None,
            ));
            probe.observe_memory_source_phases(&prime.context, observer, &fixture.directory).await?;
            assert!(fixture.outputs_are_held());
            Ok::<_, culprit_core::Error>(())
        })
        .catch_unwind(),
    )
    .await;
    let source_shutdown = probe.pool.source_shutdown();
    let closed = crate::test_support::owned_case::retire(probe.application).await;
    let result = match outcome {
        Ok(result) => result,
        Err(original) => std::panic::resume_unwind(original),
    };
    result?;
    closed?;
    if source_shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    crate::test_support::owned_case::require_active(&fixture.cancellation)?;
    fixture.control_completed();
    Ok(())
}

#[cfg(test)]
mod stack_tests;
