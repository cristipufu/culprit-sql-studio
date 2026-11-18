use super::super::test_support::RetainedScenario;
use super::*;
use crate::{
    activity::page_targets::{
        task_waits,
        tests::{CHECK_AFTER, reader},
    },
    resources::test_support::replace,
    value::Val,
};

#[tokio::test]
async fn a_withheld_token_keeps_the_actual_rechecks_null_native_fields_and_original_privacy() {
    let original = RetainedScenario::new().await;
    assert!(matches!(original.plan(true), DiagnosticFocusPlan::Ready(_)));
    let task = replace(
        &task_waits::test_task_row("0000000000000101", 1),
        &[
            ("task_resource_description", Val::Text(task_waits::test_buffer_resource(4144, "0000000B96434108"))),
            ("task_blocking_address", Val::Null),
            ("task_blocking_session_id", Val::Null),
            ("task_blocking_execution_context_id", Val::Null),
        ],
    );
    let mut replies = task_waits::test_replies(vec![task]);
    let after = &mut replies[CHECK_AFTER].as_mut().unwrap()[0];
    *after = replace(after, &[("checked_task_wait_type", Val::Null), ("checked_task_resource", Val::Null)]);
    let mut native = reader(replies);
    let captured = native.capture_page_wait_targets().await.unwrap();
    assert_eq!(native.query_count(), 10, "the original decoder/recheck roster is unchanged");
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
    let mut scenario = RetainedScenario::from_capture(captured);
    scenario.rows = original.rows;
    scenario.rebind_originals();
    assert!(scenario.page.statement_identity.is_none() && scenario.page.plan_identity.is_none());
    assert_eq!(scenario.page.after_check.as_ref().unwrap().wait, PageWaitRelation::NoLongerObserved);
    assert!(matches!(
        scenario.page.request.statement,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing }
    ));
    assert!(matches!(
        scenario.plan(true),
        DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity)
    ));
    let frame = scenario.frame(true);
    let receipt = PlannerRefusalReceipt::from_retained(
        &frame,
        &scenario.page_source,
        &scenario.page,
        DiagnosticAnalysisIssue::MissingNativeIdentity,
    );
    assert_eq!(receipt.raw_current_request_token, TokenComparison::Equal);
    assert_eq!(receipt.qualified_page_statement_token, TokenComparison::CurrentAbsent);
    let value = serde_json::to_value(&receipt).unwrap();
    assert_eq!(value["before_check"], serde_json::to_value(&scenario.page.before_check).unwrap());
    assert_eq!(value["after_check"], serde_json::to_value(&scenario.page.after_check).unwrap());
    assert_eq!(value["native_task"], serde_json::to_value(&scenario.page.task_wait).unwrap());
    assert_eq!(value["native_task"]["blocking_session_id"], serde_json::Value::Null);
    let text = serde_json::to_string(&receipt).unwrap();
    assert!(!text.contains(&scenario.rows[0].statement_identity.as_ref().unwrap().id));
    assert!(!text.contains(&scenario.references[0].manifest_sha256));
    assert!(!text.contains("SELECT"));
    let before = scenario.page.clone();
    let mut outcomes = BTreeMap::new();
    let emitted = (0..MAX_PLANNER_DIAGNOSTIC_SAMPLES + 2)
        .filter_map(|_| {
            next_receipt(
                &mut outcomes,
                &frame,
                &scenario.page_source,
                &scenario.page,
                DiagnosticAnalysisIssue::MissingNativeIdentity,
            )
        })
        .count();
    assert_eq!(emitted, MAX_PLANNER_DIAGNOSTIC_SAMPLES as usize);
    assert_eq!(outcomes["planner-refusal-diagnostic-samples"], MAX_PLANNER_DIAGNOSTIC_SAMPLES);
    assert_eq!(outcomes["missing-identity:current-page-statement-token"], MAX_PLANNER_DIAGNOSTIC_SAMPLES + 2);
    assert_eq!(scenario.page, before, "witnessing never repairs the original row");
}

#[tokio::test]
async fn the_existing_frame_verdict_distinguishes_a_negative_mars_peer_from_unknown_metadata() {
    for (lifecycle, expected) in [
        (SessionLifecycle::Unknown, GenerationPolicyWitness::NoRecordedRefusal),
        (SessionLifecycle::Connecting, GenerationPolicyWitness::RecordedRefusal),
    ] {
        let mut scenario = RetainedScenario::new().await;
        let mut peer = scenario.rows[0].clone();
        peer.key.request.as_mut().unwrap().id = "77".into();
        peer.session_context = Some(Box::new(ActivitySessionContext {
            lifecycle,
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: None,
        }));
        scenario.rows.push(peer);
        scenario.rebind_originals();
        let frame = scenario.frame(true);
        let receipt = PlannerRefusalReceipt::from_retained(
            &frame,
            &scenario.page_source,
            &scenario.page,
            DiagnosticAnalysisIssue::MissingNativeIdentity,
        );
        assert_eq!(receipt.original_request.as_ref().unwrap().generation_policy, expected);
        assert_eq!(receipt.qualified_page_statement_token, TokenComparison::Equal);
        let refused = expected == GenerationPolicyWitness::RecordedRefusal;
        assert_eq!(
            matches!(
                scenario.plan(true),
                DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity)
            ),
            refused
        );
        let mut outcomes = BTreeMap::new();
        receipt.count_missing_prerequisites(&mut outcomes);
        assert_eq!(outcomes.get("missing-identity:original-generation-policy"), refused.then_some(&1));
        let unreceipted = scenario.frame(false);
        let absent_receipt = PlannerRefusalReceipt::from_retained(
            &unreceipted,
            &scenario.page_source,
            &scenario.page,
            DiagnosticAnalysisIssue::MissingNativeIdentity,
        );
        assert!(!absent_receipt.original_request.unwrap().receipt_present);
    }
}

#[tokio::test]
async fn native_zero_sentinels_and_missing_originals_are_not_relabelled_as_holder_identity() {
    for (address, session, context) in [
        (None, None, None),
        (Some("0000000000000000"), Some(0), Some(0)),
        (Some("00000000AAAA0001"), Some(-4), Some(-1)),
    ] {
        let mut scenario = RetainedScenario::new().await;
        let task = scenario.page.task_wait.as_mut().unwrap();
        task.blocking_task_address = address.map(str::to_owned);
        task.blocking_session_id = session;
        task.blocking_execution_context_id = context;
        assert!(task.is_valid());
        let frame = scenario.frame(true);
        let value = serde_json::to_value(PlannerRefusalReceipt::from_retained(
            &frame,
            &scenario.page_source,
            &scenario.page,
            DiagnosticAnalysisIssue::MissingNativeIdentity,
        ))
        .unwrap();
        assert_eq!(value["native_task"]["blocking_task_address"], serde_json::json!(address));
        assert_eq!(value["native_task"]["blocking_session_id"], serde_json::json!(session));
        assert_eq!(value["native_task"]["blocking_execution_context_id"], serde_json::json!(context));
        scenario.rows.clear();
        scenario.rebind_originals();
        let frame = scenario.frame(true);
        let absent = PlannerRefusalReceipt::from_retained(
            &frame,
            &scenario.page_source,
            &scenario.page,
            DiagnosticAnalysisIssue::MissingNativeIdentity,
        );
        assert!(absent.original_request.is_none());
        assert_eq!(absent.raw_current_request_token, TokenComparison::OriginalAbsent);
        assert!(matches!(
            scenario.plan(true),
            DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity)
        ));
    }
}

#[tokio::test]
async fn every_unavailable_focus_keeps_its_actual_typed_issue_with_one_shared_diagnostic_ceiling() {
    let task = replace(
        &task_waits::test_task_row("0000000000000101", 1),
        &[("task_wait_type", Val::Text("PAGEIOLATCH_EX".into()))],
    );
    let mut native = reader(task_waits::test_replies(vec![task]));
    let captured = native.capture_page_wait_targets().await.unwrap();
    assert_eq!(native.query_count(), 10);
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
    let unsupported = RetainedScenario::from_capture(captured);
    let missing_identity = RetainedScenario::new().await;
    let mut changed_source = RetainedScenario::new().await;
    let SourceObservation::PageWaitTargets { database_writable, .. } = &mut changed_source.page_source.observation
    else {
        panic!()
    };
    *database_writable = database_writable.map(|writable| !writable);
    let scenarios = [
        (&unsupported, true, DiagnosticAnalysisIssue::Unsupported),
        (&missing_identity, false, DiagnosticAnalysisIssue::MissingNativeIdentity),
        (&changed_source, true, DiagnosticAnalysisIssue::SourceChanged),
    ];
    let mut outcomes = BTreeMap::new();
    let mut emitted = Vec::new();
    for (scenario, receipts, expected) in
        scenarios.into_iter().cycle().take(MAX_PLANNER_DIAGNOSTIC_SAMPLES as usize + 2)
    {
        let DiagnosticFocusPlan::Unavailable(actual) = scenario.plan(receipts) else {
            panic!("actual planner refusal")
        };
        assert_eq!(actual, expected);
        let frame = scenario.frame(receipts);
        if let Some(receipt) =
            next_receipt(&mut outcomes, &frame, &scenario.page_source, &scenario.page, actual.clone())
        {
            let value = serde_json::to_value(receipt).unwrap();
            assert_eq!(value["planner_issue"], serde_json::to_value(&actual).unwrap());
            assert_eq!(value["native_task"], serde_json::to_value(&scenario.page.task_wait).unwrap());
            emitted.push(actual);
        }
    }
    assert_eq!(emitted.len(), MAX_PLANNER_DIAGNOSTIC_SAMPLES as usize);
    assert!(emitted.contains(&DiagnosticAnalysisIssue::Unsupported));
    assert!(emitted.contains(&DiagnosticAnalysisIssue::MissingNativeIdentity));
    assert!(emitted.contains(&DiagnosticAnalysisIssue::SourceChanged));
    assert_eq!(outcomes["planner-refusal-diagnostic-samples"], MAX_PLANNER_DIAGNOSTIC_SAMPLES);
}
