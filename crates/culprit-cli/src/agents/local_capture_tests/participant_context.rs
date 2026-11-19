use super::*;
use culprit_collector::analytical::test_support::{
    current_request_work_evidence_cases, focused_request_memory_evidence, request_memory_evidence_cases,
    reservation_demand_evidence_cases, task_space_evidence_cases,
};
use culprit_engine::LocalCaptureAnalysisKind;

#[tokio::test]
async fn grant_participants_keep_native_amounts_and_checked_request_context_without_transferring_a_statement() {
    let mut cases = request_memory_evidence_cases();
    let (source, mut rows) = cases[0].clone();
    let TelemetryRecord::Operational(row) = &mut rows[0] else { panic!() };
    let OperationalFact::ExecutionGrant(grant) = &mut row.fact else { panic!() };
    grant.statement_relationship = GrantStatementRelationship::Unresolved;
    grant.requested_kibibytes = Some("9007199254740993".into());
    cases.push((source, rows));
    for (source, rows) in cases {
        let expected = rows.clone();
        let context = NativeReadContext::from_snapshot(&source).unwrap();
        let observations = expected
            .iter()
            .map(|row| {
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::ExecutionGrant(grant),
                    ..
                }) = row
                else {
                    panic!("current grant")
                };
                grant.as_ref()
            })
            .collect::<Vec<_>>();
        let expected_report = culprit_checks::request_memory::explain(
            culprit_checks::request_memory::RequestMemoryEvidence { context: &context, observations: &observations },
            None,
        )
        .unwrap();
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::RequestMemory).await;
        assert!(!error, "{value}");
        for entry in value["request_memory"]["entries"].as_array().unwrap() {
            let point = expected
                .iter()
                .find_map(|row| {
                    let TelemetryRecord::Operational(row) = row else { return None };
                    let OperationalFact::ExecutionGrant(grant) = &row.fact else { return None };
                    (json!(grant.observed_at) == entry["native_observation"]["observed_at"]
                        && json!(grant.state) == entry["native_observation"]["state"]
                        && json!(grant.request_association) == entry["native_observation"]["request_association"])
                        .then_some(grant)
                })
                .unwrap();
            let native = &entry["native_observation"];
            assert_eq!(native["requested_kibibytes"], json!(point.requested_kibibytes));
            assert_eq!(native["granted_kibibytes"], json!(point.granted_kibibytes));
            assert_eq!(native["used_kibibytes"], json!(point.used_kibibytes));
            let qualified = &expected_report
                .entries
                .iter()
                .find(|candidate| candidate.observation_identity == point.identity)
                .unwrap()
                .qualified_request;
            assert_eq!(entry["qualified_request"], json!(qualified));
            let context = &entry["participant_context"];
            if let Some(request) = point.request.as_ref().filter(|request| qualified.as_ref() == Some(&request.key)) {
                assert_eq!(context["actor"], json!(request.key));
                assert_eq!(context["client"], json!(request.client));
                assert_eq!(context["observed_at"], point.observed_at);
                if point.statement_relationship == GrantStatementRelationship::ObservedCurrentStatement {
                    assert_eq!(context["statement"]["availability"], "available");
                    assert!(!context["statement"]["preview"].as_str().unwrap().contains("9264"));
                } else {
                    assert!(context["statement"].is_null(), "a checked request is not a checked current statement");
                }
            } else {
                assert!(context.is_null(), "another visible actor cannot supply a changed request's context");
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn focused_memory_keeps_the_exact_original_and_later_participant_clocks_independent_and_withholds_changed_links()
{
    for state in [FocusSourceState::Matched, FocusSourceState::Changed, FocusSourceState::Unverified] {
        let (mut source, mut rows) = focused_request_memory_evidence();
        let SourceObservation::FocusedRequestMemory { snapshot, result } = &mut source.observation else { panic!() };
        let original = result.focus.original.clone();
        let original_at = result.focus.original_observed_at.clone();
        let followup_at = snapshot.observed_from.clone();
        assert_ne!(original_at, followup_at);
        if state != FocusSourceState::Matched {
            result.source = state;
            result.membership = RequestMemoryMembership::Unverified;
            result.subject = None;
            result.resource = None;
            result.peers.clear();
            snapshot.records = 0;
            snapshot.sources = vec![StateSourceCoverage::unavailable(
                snapshot.sources[0].source.clone(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            )];
            rows.clear();
        }
        assert!(source.observation.covers_request());
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::RequestMemory).await;
        assert!(!error, "{state:?}:{value}");
        let report = &value["request_memory"];
        assert_eq!(report["focus_source"], json!(state));
        let context = &report["original_participant_context"];
        assert_eq!(context.is_object(), state == FocusSourceState::Matched);
        if state == FocusSourceState::Matched {
            assert_eq!(context["actor"], json!(original.key));
            assert_eq!(context["client"], json!(original.client));
            assert_eq!(context["observed_at"], original_at);
            assert_eq!(report["entries"][0]["participant_context"]["observed_at"], followup_at);
        } else {
            assert!(!value.to_string().contains("private-memory-login"));
            assert!(!value.to_string().contains("private-memory-host"));
        }
        for entry in report["entries"].as_array().unwrap() {
            assert!(!entry["explanation"].as_str().unwrap().contains("9264"));
            if let Some(preview) = entry["participant_context"]["statement"]["preview"].as_str() {
                assert!(!preview.contains("9264"));
            }
        }
        assert!(!value.to_string().contains("statement_identity"));
        assert!(!value.to_string().contains("manifest_sha256"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn task_participant_context_requires_both_original_checks_and_preserves_exact_independent_counters() {
    for (source, rows) in task_space_evidence_cases() {
        let expected = rows.clone();
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::TaskSpace).await;
        assert!(!error, "{value}");
        for entry in value["task_space"]["entries"].as_array().unwrap() {
            let point = expected
                .iter()
                .find_map(|row| {
                    let TelemetryRecord::Operational(row) = row else { return None };
                    let OperationalFact::TaskSpace(task) = &row.fact else { return None };
                    (json!(task.task) == entry["task"]).then_some(task)
                })
                .unwrap();
            let native = &entry["native_observation"];
            assert_eq!(native["bytes_per_page"], point.bytes_per_page);
            assert_eq!(native["user_allocated_pages"], json!(point.user_allocated_pages));
            assert_eq!(native["user_deallocated_pages"], json!(point.user_deallocated_pages));
            assert_eq!(native["internal_allocated_pages"], json!(point.internal_allocated_pages));
            assert_eq!(native["internal_deallocated_pages"], json!(point.internal_deallocated_pages));
            assert_eq!(native["request_context"], json!(point.request_context()));
            let context = &entry["participant_context"];
            if point.request_context() == TaskRequestContext::CheckedAtBounds {
                let request = point.request.as_ref().unwrap();
                assert_eq!(context["actor"], json!(request.key));
                assert_eq!(context["client"], json!(request.client));
                assert_eq!(context["statement"]["availability"], "available");
                assert!(!context["statement"]["preview"].as_str().unwrap().contains("9264"));
            } else {
                assert!(context.is_null(), "a task tuple or native join cannot recover a changed or missing statement");
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn focused_work_keeps_original_client_context_separate_from_the_native_point_and_withholds_unverified_links() {
    for (source, rows) in current_request_work_evidence_cases() {
        let SourceObservation::CurrentRequestWork { result, .. } = &source.observation else { panic!() };
        let focus = result.focus.as_ref().unwrap();
        let original_at = focus.original_observed_at.clone();
        let original_actor = focus.original.key.clone();
        let binding = result.binding;
        let point_statements = rows
            .iter()
            .filter_map(|row| {
                let TelemetryRecord::Operational(row) = row else { return None };
                let OperationalFact::CurrentRequestWork(point) = &row.fact else { return None };
                Some(point.statement.clone())
            })
            .collect::<Vec<_>>();
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::CurrentRequestWork)
                .await;
        assert!(!error, "{value}");
        let view = &value["current_request_work"];
        let original = &view["original_participant_context"];
        assert_eq!(original.is_object(), binding == FocusedRequestBinding::Matched);
        if binding == FocusedRequestBinding::Matched {
            assert_eq!(original["actor"], json!(original_actor));
            assert_eq!(original["observed_at"], original_at);
            assert_eq!(original["client"]["user"], "private-current-work-login");
            assert_eq!(original["client"]["host"], "private-current-work-client");
        } else {
            assert!(!value.to_string().contains("private-current-work-login"));
            assert!(!value.to_string().contains("private-current-work-client"));
        }
        let entries = view["entries"].as_array().unwrap();
        assert_eq!(entries.len(), point_statements.len());
        for (entry, payload) in entries.iter().zip(point_statements) {
            let DefinitionPayload::Available { text, .. } = payload else { panic!() };
            let statement = &entry["statement"];
            assert_eq!(statement["preview"], text.chars().take(512).collect::<String>());
            assert_eq!(statement["preview_truncated"], text.chars().count() > 512);
            assert_ne!(entry["observed_at"], original_at);
        }
        assert!(!value.to_string().contains("private-current-work-sql"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn reservation_demand_mcp_replays_canonical_original_waiter_advice_without_a_native_requery() {
    for (index, (source, rows)) in reservation_demand_evidence_cases().into_iter().enumerate() {
        let context = NativeReadContext::from_snapshot(&source).unwrap();
        let observations = rows
            .iter()
            .map(|row| {
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::ExecutionGrant(grant),
                    ..
                }) = row
                else {
                    panic!("reservation-demand fixture")
                };
                grant.as_ref()
            })
            .collect::<Vec<_>>();
        let native = &source.observation.snapshot().unwrap().sources[0];
        let expected = culprit_checks::request_memory::explain(
            culprit_checks::request_memory::RequestMemoryEvidence { context: &context, observations: &observations },
            Some(&culprit_core::SourceOutcome {
                source: native.source.clone(),
                state: native.state,
                reason: None,
                reason_message: None,
            }),
        )
        .unwrap();
        let expected_waiter =
            expected.entries.iter().find(|entry| entry.observation_identity == observations[0].identity).unwrap();
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::RequestMemory).await;
        assert!(!error, "{value}");
        let report = &value["request_memory"];
        let entry = &report["entries"][0];
        assert_eq!(entry["explanation"], expected_waiter.explanation);
        assert_eq!(entry["investigation"], expected_waiter.investigation);
        assert_eq!(entry["verify"], expected_waiter.verify);
        assert_eq!(entry["qualified_request"], json!(expected_waiter.qualified_request));
        assert_eq!(entry["native_observation"]["state"], "waiting");
        assert_eq!(entry["native_observation"]["resource_observation"]["grantee_count"], "4");
        assert_eq!(entry["native_observation"]["resource_observation"]["granted_kibibytes"], "16384");
        assert_eq!(report["source_state"], if index == 2 { "partial" } else { "collected" });
        assert!(
            report["limitations"]
                .as_array()
                .unwrap()
                .contains(&json!(culprit_core::t!("view.request_memory.demand.limit")))
        );
        assert!(!entry["explanation"].as_str().unwrap().contains("9007199254740993"));
        assert!(!entry["explanation"].as_str().unwrap().contains("9264"));
        assert!(!entry["participant_context"]["statement"]["preview"].as_str().unwrap().contains("9264"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
