//! An original lock incident is independent evidence alongside the completed scalar access work.
//! Fixture coordination never turns client submission, elapsed time or a plan shape into lock ownership.
use super::*;
use culprit_core::{ScenarioKind, plan_compare::NativeRuntimeInputRelation};
use serde_json::json;
use tokio::sync::Notify;

const SUBMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

struct HeldBlocker {
    client: MsClient,
    actor: NativeActivityIdentity,
}
impl HeldBlocker {
    async fn open(fixture: &OwnedSourceFixture, database: &str) -> Self {
        let mut client = crate::test_support::connect(
            &config(fixture.port, database, fixture.password(), "scalar-original-blocker"),
            SessionKind::User,
        )
        .await
        .unwrap();
        let prepared = AssertUnwindSafe(async {
            verify_owned_source(&mut client).await.unwrap();
            let native = crate::conn::query_one(
                &mut client,
                "SELECT /* culprit fixture */ @@SPID AS id,CONVERT(varchar(40),CAST(login_time AS datetime2),126) AS generation FROM sys.dm_exec_sessions WHERE session_id=@@SPID",
            )
            .await
            .unwrap();
            let actor = NativeActivityIdentity {
                id: crate::read::signed_integer(&native, "id").unwrap().to_string(),
                generation: crate::sql_time::native_time(&crate::read::text(&native, "generation").unwrap()).unwrap(),
            };
            // Only a lock is planted: no row, statistic, index, procedure, input or plan policy is changed.
            batch(&mut client, "BEGIN TRAN;SELECT id FROM dbo.scalar_rows WITH(TABLOCKX,HOLDLOCK) WHERE id=1").await;
            actor
        })
        .catch_unwind()
        .await;
        match prepared {
            Ok(actor) => Self { client, actor },
            Err(original) => {
                let _ = client.close().await;
                std::panic::resume_unwind(original);
            }
        }
    }

    async fn capture_and_release(
        mut self,
        owner: &mut OwnedCaptures,
        submitting: &Notify,
        waiter: &probe::Actor,
        statement: &CachedStatementContext,
    ) -> Captured {
        let captured = AssertUnwindSafe(async {
            // No polling or repeated source capture: this signal only schedules the one native observation.
            tokio::time::timeout(SUBMISSION_TIMEOUT, submitting.notified()).await.unwrap();
            let capture = owner.capture(CollectionSelection::Activity).await.unwrap();
            require_original_blocking(&capture, waiter, &self.actor, statement);
            capture
        })
        .catch_unwind()
        .await;
        // Keep the first error, but always await rollback and this same client close before propagating it.
        let released = AssertUnwindSafe(batch(&mut self.client, "ROLLBACK")).catch_unwind().await;
        let closed = self.client.close().await;
        let captured = match captured {
            Ok(captured) => captured,
            Err(original) => std::panic::resume_unwind(original),
        };
        if let Err(original) = released {
            std::panic::resume_unwind(original);
        }
        closed.unwrap();
        captured
    }
}

fn actor_identity(actor: &probe::Actor) -> NativeActivityIdentity {
    NativeActivityIdentity {
        id: actor.id.to_string(),
        generation: crate::sql_time::native_time(&actor.generation).unwrap(),
    }
}

fn require_original_blocking(
    capture: &Captured,
    actor: &probe::Actor,
    blocker: &NativeActivityIdentity,
    statement: &CachedStatementContext,
) {
    capture
        .with_activity_frame(|frame| {
            assert!(frame.read_complete() && frame.context().has_qualified_local_owner());
            assert_eq!(frame.context().database_writable, Some(true));
            let actor = actor_identity(actor);
            assert!(frame.has_session_generation_authority(&actor));
            assert!(frame.has_session_generation_authority(blocker));
            let requests = frame.session_rows(&actor).collect::<Vec<_>>();
            let [request] = requests.as_slice() else { panic!("one original native scalar request") };
            let row = request.observation;
            assert!(row.key.request.is_some() && row.waiting_on_lock == Some(true));
            assert_eq!(row.scope, ActivityScopeBasis::RequestDatabase);
            let token = row.statement_identity.as_ref().unwrap();
            assert_eq!(token.id, statement.identity.sql_handle.trim_start_matches("0x"));
            assert_eq!(token.qualifiers["database_id"], statement.database_id.to_string());
            assert_eq!(
                token.qualifiers["statement_start_offset"],
                statement.identity.statement_start_bytes.to_string()
            );
            assert_eq!(token.qualifiers["statement_end_offset"], statement.identity.statement_end_bytes.to_string());
            assert_eq!(
                row.query_hash.as_deref(),
                statement.query_hash.as_deref().map(|hash| hash.trim_start_matches("0x"))
            );
            assert!(request.reference.is_some());
            let owners = frame.session_rows(blocker).collect::<Vec<_>>();
            let [owner] = owners.as_slice() else { panic!("one observed native blocker generation") };
            assert!(owner.observation.idle && owner.observation.key.request.is_none());
            assert!(
                owner.observation.key.transaction.is_some()
                    && owner.observation.open_transactions.is_some_and(|n| n > 0)
            );
            let edges = ActivityEvidence::blocking_for_frame(frame);
            let edges = edges.iter().filter(|edge| edge.waiter == row.key).collect::<Vec<_>>();
            let [edge] = edges.as_slice() else { panic!("one recorded native lock owner") };
            assert_eq!(edge.owner.session().as_ref(), Some(blocker));
            assert!(edge.owner_observed);
            let graph = culprit_core::blocking::analyze_activity(frame)?.unwrap();
            assert!(
                graph.chains.iter().any(|chain| chain
                    .tree
                    .owner
                    .as_ref()
                    .and_then(ActivityWaitOwner::session)
                    .as_ref()
                    == Some(blocker))
            );
            Ok(())
        })
        .unwrap();
}

async fn competing_blocking(fixture: &OwnedSourceFixture, database: &str) {
    let directory = crate::test_support::receipt_directory();
    let mut probe = Probe::counted(fixture, database, IndexCoverage::LookupRequired).await;
    eprintln!("owned scalar blocking phase=prime");
    let initial = probe.record(2, CacheExpectation::New).await;
    let actor = probe.actor();
    let blocker = HeldBlocker::open(fixture, database).await;
    eprintln!("owned scalar blocking phase=lock-held");
    let blocker_actor = blocker.actor.clone();
    let submitting = Notify::new();
    let mut owner = OwnedCaptures::from_pool(probe.pool.clone(), probe.port).unwrap();
    // Both futures stay borrowed in this same fixture body. Neither actor nor observer is detached, and
    // the retained execution still uses the existing 40-second capture/attention/retirement owner.
    let (run, blocked) = tokio::join!(
        AssertUnwindSafe(probe.record_native_notifying(1, CacheExpectation::Reused(&initial.context), &submitting))
            .catch_unwind(),
        AssertUnwindSafe(blocker.capture_and_release(&mut owner, &submitting, &actor, &initial.context)).catch_unwind(),
    );
    let original = match run {
        Ok(run) => run,
        Err(original) => std::panic::resume_unwind(original),
    };
    let blocked = match blocked {
        Ok(capture) => capture,
        Err(original) => std::panic::resume_unwind(original),
    };
    eprintln!("owned scalar blocking phase=original-execution-and-blocking-capture-settled");
    assert_context(&original, ParameterValues::Different);
    let mut retained = Original::capture_owned(owner, &original).await;
    eprintln!("owned scalar blocking phase=original-plan-retained");
    assert_eq!(blocked.source().unwrap().target, retained.capture.source().unwrap().target);
    assert_eq!(blocked.source().unwrap().partition, retained.capture.source().unwrap().partition);
    let recovered = retained.owner.capture(CollectionSelection::Activity).await.unwrap();
    recovered
        .with_activity_frame(|frame| {
            let actor = actor_identity(&actor);
            let rows = frame.session_rows(&actor).collect::<Vec<_>>();
            let [row] = rows.as_slice() else { panic!("the same original actor remains after execution completion") };
            assert!(row.observation.key.request.is_none() && row.observation.wait.is_none());
            assert!(!ActivityEvidence::blocking_for_frame(frame).iter().any(|edge| edge.waiter.session == actor));
            assert!(frame.session_rows(&blocker_actor).next().is_none(), "the original blocker is physically closed");
            Ok(())
        })
        .unwrap();
    eprintln!("owned scalar blocking phase=recovered-activity-verified");
    // This separately paid intervention changes the native cached entry, never the original incident.
    batch(&mut probe.application, "EXEC sys.sp_recompile N'dbo.scalar_probe'").await;
    eprintln!("owned scalar blocking phase=counterpart-execution");
    let counterpart = probe.record_native(1, CacheExpectation::New).await;
    eprintln!("owned scalar blocking phase=counterpart-execution-and-metadata-verified");
    assert_eq!(original.result, counterpart.result);
    let pair = crate::plan_xml::runtime_input_pair(&original.plan, &counterpart.plan);
    assert_eq!(pair.relation(), NativeRuntimeInputRelation::Same);
    let lookup = original.operator("Key Lookup");
    let scan = counterpart.operator("Clustered Index Scan");
    let lookup_reads = original.logical_reads("Key Lookup");
    let scan_reads = counterpart.logical_reads("Clustered Index Scan");
    assert!(lookup_reads >= 1000.0 && scan_reads < lookup_reads);
    let comparison =
        culprit_core::plan_compare::compare_native_pair(pair, "Blocked original", "Scan", &Default::default());
    let observed = comparison
        .scenarios
        .iter()
        .find(|scenario| {
            scenario.kind == ScenarioKind::Parameters && scenario.a == Some(lookup.id) && scenario.b == Some(scan.id)
        })
        .unwrap();
    assert!(observed.detail.as_ref().unwrap().contains(&culprit_core::t!("core.plan_compare.scalar_reuse.limit")));
    assert!(
        culprit_core::insights::insights(&original.plan)
            .iter()
            .filter(|insight| insight.rule == "parameter-context")
            .all(|insight| insight.severity == Severity::Low && insight.impact == 0.0 && insight.fix_sql.is_none())
    );
    // The record itself, native edge and completed access work survive both original actors disappearing.
    batch(&mut probe.application, "DROP PROCEDURE dbo.scalar_probe").await;
    probe.application.close().await.unwrap();
    retained.verify_after_actor_disappears(fixture, database, actor.clone()).await;
    for capture in [&blocked, &recovered] {
        retained.owner.verify_retained(capture).await.unwrap();
    }
    require_original_blocking(&blocked, &actor, &blocker_actor, &original.context);
    assert_eq!(
        crate::plan_xml::runtime_input_pair(&retained.explain(), &counterpart.plan).relation(),
        NativeRuntimeInputRelation::Unavailable
    );
    let captures = [&blocked, &retained.capture, &recovered];
    let source_queries = captures.iter().map(|capture| capture.manifest.source_queries).sum::<u32>();
    assert!(source_queries <= 26, "two original seven-call Activity captures plus the twelve-call retained-plan cap");
    crate::test_support::save_cost_receipt(
        &directory,
        "scalar-original-blocking-control",
        &json!({
            "successful": true,
            "captures": captures.iter().map(|capture| &capture.manifest).collect::<Vec<_>>(),
            "original_activity_records": &blocked.records,
            "original_retained_records": &retained.capture.records,
            "recovered_activity_records": &recovered.records,
            "source_queries": source_queries,
            "source_dispatch_cap": 26,
            "separately_paid_fixture_dispatch_cap": 48,
            "fixture_caps_are_not_observed_server_work": true,
            "lookup_operator_reads": lookup_reads,
            "scan_operator_reads": scan_reads,
            "comparison": comparison,
            "blocking_scope": "original-native-request-and-observed-session-generations",
            "elapsed_time_attribution": false,
            "exclusive_parameter_cause": false,
            "native_waitstats_bridge_available": false,
            "native_cost_qualified": false,
        }),
    )
    .unwrap();
}

#[tokio::test]
#[ignore = "requires SQL2022 nonce-owned loopback fixture; original blocking and independent completed work"]
async fn owned_scalar_blocking_keeps_original_lock_incident_and_completed_access_work_after_both_actors_disappear() {
    super::super::super::fixture::run("scalar_blocked", competing_blocking).await;
}
