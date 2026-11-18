use super::super::test_support::RetainedScenario;
use super::*;
use crate::{
    activity::page_targets::{
        holders::{test_focus, test_replies},
        task_waits,
        tests::{page_wait, reader, successful_replies},
    },
    resources::test_support::replace,
    value::Val,
};

#[tokio::test]
async fn compatibility_requires_an_actual_original_task_without_converting_an_unresolved_holder_into_a_positive() {
    let scenario = RetainedScenario::new().await;
    let roster = scenario.frame(true).page_wait_task_requests().unwrap();
    let mut evidence = NativeHolderEvidence::default();
    assert!(evidence.require_compatibility().is_err());
    assert!(evidence.require_qualified_holder().is_err());
    evidence.observe_page(&scenario.page, &roster).unwrap();
    assert_eq!(evidence.native_task_waits(), 1);
    evidence.require_compatibility().unwrap();
    assert!(!evidence.has_qualified_holder());
    assert!(evidence.require_qualified_holder().is_err());

    for changed in ["session", "request", "statement"] {
        let mut page = scenario.page.clone();
        match changed {
            "session" => page.request.key.session.id = "99".into(),
            "request" => page.request.key.request.as_mut().unwrap().generation = "other-request-generation".into(),
            _ => page.request.statement_identity.as_mut().unwrap().id = "other-statement".into(),
        }
        assert!(evidence.observe_page(&page, &roster).is_err(), "{changed}");
        assert_eq!(evidence.native_task_waits(), 1, "a rejected actor cannot become native compatibility evidence");
    }
}

#[tokio::test]
async fn coordinator_rows_and_physical_io_tasks_cannot_satisfy_native_page_latch_compatibility() {
    let mut coordinator = reader(successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]));
    let captured = coordinator.capture_page_wait_targets().await.unwrap();
    assert_eq!(coordinator.close().await, SourceShutdown::Quiescent);
    let coordinator = RetainedScenario::from_capture(captured);
    assert!(coordinator.page.task_wait.is_none());
    let io_task = replace(
        &task_waits::test_task_row("0000000000000101", 1),
        &[("task_wait_type", Val::Text("PAGEIOLATCH_EX".into()))],
    );
    let mut native = reader(task_waits::test_replies(vec![io_task]));
    let captured = native.capture_page_wait_targets().await.unwrap();
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
    let io = RetainedScenario::from_capture(captured);
    assert_eq!(io.page.task_wait.as_ref().unwrap().resource.kind, culprit_core::WaitResourceKind::PageIo);
    let mut evidence = NativeHolderEvidence::default();
    for scenario in [coordinator, io] {
        assert!(scenario.page.is_valid());
        let roster = scenario.frame(true).page_wait_task_requests().unwrap();
        evidence.observe_page(&scenario.page, &roster).unwrap();
    }
    assert!(evidence.require_compatibility().is_err());
    assert_eq!(evidence.native_task_waits(), 0);
}

#[tokio::test]
async fn every_actual_qualified_point_still_requires_the_structured_pointer_and_exact_guarded_request() {
    let focus = test_focus().await;
    let mut native = reader(test_replies());
    let captured = native.capture_page_task_holders(&focus).await.unwrap();
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
    assert_eq!(native.query_count(), 15);
    assert!(focus.accepts_observation(&captured.context, &captured.observation));
    let points = validate_actual_points(&captured.observation, &captured.context).unwrap();
    assert_eq!(points, 2);
    let mut evidence = NativeHolderEvidence::default();
    let scenario = RetainedScenario::new().await;
    evidence.observe_page(&scenario.page, &scenario.frame(true).page_wait_task_requests().unwrap()).unwrap();
    evidence.observe_qualified_points(points);
    evidence.require_qualified_holder().unwrap();

    for changed in ["pointer", "request", "statement", "self", "role"] {
        let mut observation = captured.observation.clone();
        let mut context = captured.context.clone();
        match changed {
            "pointer" => observation.before.holder.task.as_mut().unwrap().task_address = "00000000BBBB0001".into(),
            "request" => observation.before.holder.request.as_mut().unwrap().key.request = None,
            "statement" => observation.before.holder.request.as_mut().unwrap().statement_identity = None,
            "self" => {
                let task = observation.page.as_ref().unwrap().task_wait.as_ref().unwrap();
                observation.before.holder.task.as_mut().unwrap().task_address = task.task_address.clone();
            }
            _ => context.database_writable = context.database_writable.map(|writable| !writable),
        }
        assert!(validate_actual_points(&observation, &context).is_err(), "{changed}");
    }
}

#[tokio::test]
async fn sampled_native_coverage_is_allowed_but_a_failed_or_stopped_source_read_is_never_compatibility() {
    let scenario = RetainedScenario::new().await;
    require_readable_source(&scenario.activity_source).unwrap();
    require_readable_source(&scenario.page_source).unwrap();
    let mut metrics = ReadWorkMetrics {
        active_tasks: 0,
        untracked_tasks: 0,
        native_session_work: None,
        templates: vec![ReadQueryMetrics {
            template_id: digest(b"original-native-read"),
            statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted },
            completed_tasks: 1,
            failures: 0,
            timeouts: 0,
            cancellations: 0,
            unconfirmed_cancellations: 0,
            rows_decoded: "1".into(),
            charged_value_bytes: "1".into(),
            client_elapsed_ms: 1.0,
        }],
    };
    assert!(metrics.is_valid());
    require_successful_reads(&metrics).unwrap();
    metrics.templates[0].failures = 1;
    assert!(metrics.is_valid(), "a fully received source failure can have confirmed close");
    assert!(require_successful_reads(&metrics).is_err());
    for coverage in [
        StateSourceCoverage::unavailable(
            scenario.page_source.source.clone(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        ),
        StateSourceCoverage::not_applicable(scenario.page_source.source.clone()),
        StateSourceCoverage {
            stopped: Some(CollectionStop::SourceUnavailable),
            ..StateSourceCoverage::observed(scenario.page_source.source.clone(), 1, false)
        },
        StateSourceCoverage {
            error_code: Some(culprit_core::ErrorCode::PermissionDenied),
            ..StateSourceCoverage::observed(scenario.page_source.source.clone(), 1, false)
        },
    ] {
        let mut source = scenario.page_source.clone();
        let SourceObservation::PageWaitTargets { snapshot, .. } = &mut source.observation else { panic!() };
        snapshot.sources = vec![coverage];
        assert!(require_readable_source(&source).is_err());
    }
}
