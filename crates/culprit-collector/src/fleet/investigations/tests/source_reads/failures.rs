use super::*;

#[tokio::test]
async fn unqualified_or_refused_native_history_cost_never_constructs_a_source() {
    for unqualified in [true, false] {
        let mut runtime = Runtime::from_fixture(fixture().await).await;
        gateway_answer(&mut runtime).await;
        if unqualified {
            runtime.fixture.backend.additional_recipes[0].cost = RecipeCostQualification::Unqualified;
        } else {
            runtime.fixture.backend.accept = false;
        }
        let before = runtime.native_calls();
        assert!(!runtime.enroll().await);
        assert_eq!(runtime.native_calls(), before);
        assert!(episode(&mut runtime).await.source_read.unwrap().needs_work());
    }
}

#[tokio::test]
async fn quiet_original_wait_retains_owed_context_and_does_not_reopen_the_gateway_question() {
    let mut runtime = Runtime::from_fixture(fixture().await).await;
    let original = gateway_answer(&mut runtime).await;
    runtime.complete_empty_baseline().await;
    let quiet = episode(&mut runtime).await;
    assert_eq!(quiet.lifecycle, DiagnosticLifecycle::Resolved);
    assert!(quiet.source_read.as_ref().unwrap().needs_work());
    assert_eq!(quiet.report, original.report);
    assert!(runtime.enroll().await);
    let intents = runtime.fixture.journal.focused_intents().await.unwrap();
    assert!(intents.iter().all(|intent| matches!(intent.focus, DiagnosticFocus::SourceRead { .. })));
}

#[tokio::test]
async fn route_or_updateability_change_after_enrollment_dispatches_zero_history_body_work() {
    for changed in ["route", "updateability"] {
        let mut runtime = Runtime::from_fixture(fixture().await).await;
        gateway_answer(&mut runtime).await;
        assert!(runtime.enroll().await);
        let job = runtime.take_focus().await;
        let before = runtime.native_calls();
        if changed == "route" {
            runtime.fixture.backend.changed.store(true, Ordering::SeqCst);
        } else {
            let mut source = runtime.fixture.backend.activity.source.clone();
            let SourceObservation::Activity(frame) = &mut source.observation else { panic!() };
            frame.database_writable = Some(false);
            runtime.investigations.bindings.observe(&job.target, &source, &runtime.fixture.backend.revision).unwrap();
        }
        let result = runtime.execute(&job).await;
        assert_ne!(result.event.status, CollectionStatus::Complete);
        assert_eq!(runtime.native_calls(), before, "{changed}");
    }
}

#[tokio::test]
async fn cancelled_or_unconfirmed_history_preserves_owner_debt_and_never_enrolls_a_result_control() {
    use crate::fleet::{admission::AdmittedBackend, storage::CaptureStorage};
    use tokio::time::Instant;
    for stop in [CollectionStop::Cancelled, CollectionStop::CancellationUnconfirmed] {
        let mut runtime = Runtime::from_fixture(fixture().await).await;
        let original = gateway_answer(&mut runtime).await;
        assert!(runtime.enroll().await);
        let job = runtime.take_focus().await;
        runtime.fixture.backend.focus_failure = Some(stop);
        let admitted = AdmittedBackend::new(&runtime.fixture.backend, Instant::now());
        let result = runtime
            .investigations
            .execute(
                &runtime.fixture.backend,
                &admitted,
                &runtime.fixture.journal,
                &mut CaptureStorage::local(&mut runtime.fixture.outbox),
                &job,
                &CollectionCancellation::default(),
            )
            .await;
        if stop == CollectionStop::CancellationUnconfirmed {
            assert_eq!(CollectionStop::from_error(&result.err().unwrap()), stop);
            assert_eq!(admitted.source_work(), SourceWorkState::Unconfirmed);
            assert_eq!(runtime.fixture.outbox.status(100, None).await.unwrap().unconfirmed_sources, 1);
            assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().contains(&job.key));
        } else {
            let result = result.unwrap();
            assert!(matches!(result.outcome, Outcome::FocusedRetry { source_work: SourceWorkState::Quiescent, .. }));
        }
        let current = episode(&mut runtime).await;
        assert_eq!(current.report, original.report);
        assert_eq!(current.analysis, original.analysis);
        assert!(current.source_read_capture().is_none());
        assert!(matches!(current.source_read.unwrap().state, SourceReadStageState::Stopped { .. }));
        assert!(runtime.fixture.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn flat_history_intent_obeys_existing_request_and_manifest_bytes_before_constructing_native_source() {
    let mut fixture = fixture().await;
    let RecipeCostQualification::Measured(receipt) = &mut fixture.backend.additional_recipes[0].cost else { panic!() };
    let prototype = receipt.read_metrics.templates[0].clone();
    // Grow only the separate history qualification, after the ordinary original/gateway boundary. Three
    // completed templates stay within the existing tested query budget. Legal quoted native identifiers
    // exercise serialized bytes independently of text length; no preview is copied into the request focus.
    receipt.read_metrics.templates = (0..3)
        .map(|template| {
            let quoted_identifier = "\"".repeat(90);
            let columns = (0..70)
                .map(|column| format!("NULL AS [read_{template}_{column:02}_{quoted_identifier}]"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!("SELECT {columns}");
            let mut metric = prototype.clone();
            metric.completed_tasks = 1;
            metric.template_id = digest(sql.as_bytes());
            metric.statement = DefinitionPayload::sql(&sql, culprit_core::Engine::Mssql);
            let DefinitionPayload::Available { text, .. } = &metric.statement else { panic!() };
            assert!(text.len() <= MAX_READ_TEMPLATE_TEXT_BYTES);
            metric
        })
        .collect();
    assert!(receipt.read_metrics.is_valid());
    assert_eq!(
        receipt.read_metrics.templates.iter().map(|metric| metric.completed_tasks).sum::<u32>(),
        receipt.tested_budget.max_queries
    );
    assert!(receipt.is_valid(), "the independent synthetic history approval remains structurally bounded");
    let (_, rows) = crate::analytical::test_support::compiler_admission_evidence();
    let template = rows
        .into_iter()
        .find_map(|row| match row {
            TelemetryRecord::Operational(row) if matches!(row.fact, OperationalFact::CounterSet(_)) => Some(row),
            _ => None,
        })
        .unwrap();
    fixture.backend.compiler_gateways = Some(
        (0..MAX_FOCUSED_COMPILER_GATEWAYS)
            .map(|pool| {
                let mut row = template.clone();
                let OperationalFact::CounterSet(set) = &mut row.fact else { panic!() };
                set.identity.qualifiers.insert("resource_pool_id".into(), pool.to_string());
                row.key = set.key().unwrap();
                assert!(CompilerGatewayFacts::from_observation(&row).is_ok());
                row
            })
            .collect(),
    );
    let mut runtime = Runtime::from_fixture(fixture).await;
    gateway_answer(&mut runtime).await;
    assert!(runtime.enroll().await);
    let intent = runtime
        .fixture
        .journal
        .focused_intents()
        .await
        .unwrap()
        .into_iter()
        .find(|intent| matches!(intent.focus, DiagnosticFocus::SourceRead { .. }))
        .unwrap();
    let request = CollectionRequest {
        subject: intent.subject(),
        selection: intent.selection().unwrap(),
        budget: intent.budget.clone(),
    };
    request.validate().unwrap();
    let request_bytes = serde_json::to_vec(&request).unwrap().len();
    assert!(request_bytes <= crate::collection_files::MAX_MANIFEST_BYTES);
    let lineage = DiagnosticInvestigationLineage::from_intent(&intent).unwrap();
    assert!(
        request_bytes + serde_json::to_vec(&lineage).unwrap().len() > crate::collection_files::MAX_MANIFEST_BYTES,
        "both genuine bounded prerequisites and execution/cost ownership remain in the original intent"
    );
    let job = runtime.take_focus().await;
    let before = runtime.native_calls();
    let result = runtime.execute(&job).await;
    assert_ne!(result.event.status, CollectionStatus::Complete);
    assert_eq!(&runtime.native_calls()[1..], &before[1..], "manifest refusal precedes factory open/describe");
    assert!(episode(&mut runtime).await.source_read_capture().is_none());
}

#[tokio::test]
async fn missing_foreign_or_tampered_prerequisite_is_refused_before_reservation_and_native_construction() {
    let mut runtime = Runtime::from_fixture(fixture().await).await;
    gateway_answer(&mut runtime).await;
    assert!(runtime.enroll().await);
    let intent = runtime
        .fixture
        .journal
        .focused_intents()
        .await
        .unwrap()
        .into_iter()
        .find(|intent| matches!(intent.focus, DiagnosticFocus::SourceRead { .. }))
        .unwrap();
    let before = runtime.native_calls();
    let entries = runtime.fixture.outbox.status(100, None).await.unwrap().entries.len();
    for defect in ["missing-capture", "foreign-scope", "tampered-body", "ordinal"] {
        let mut request = CollectionRequest {
            subject: intent.subject(),
            selection: intent.selection().unwrap(),
            budget: intent.budget.clone(),
        };
        let CollectionSelection::SourceRead { focus, .. } = &mut request.selection else { panic!() };
        let SourceReadPrerequisite::CompilationAdmission { report } = &mut focus.prerequisite else { panic!() };
        match defect {
            "missing-capture" => {
                let changed = "f".repeat(32);
                report.observation.capture.observation_id = changed.clone();
                report.observation.batch_id = batch_id(&changed, report.observation.stream).unwrap();
                for reference in &mut report.gateways {
                    reference.capture.observation_id = changed.clone();
                    reference.batch_id = batch_id(&changed, reference.stream).unwrap();
                }
            }
            "foreign-scope" => {
                let scope = digest(b"another-authenticated-logical-scope");
                report.observation.source_scope_id = scope.clone();
                for reference in &mut report.gateways {
                    reference.source_scope_id = scope.clone();
                }
            }
            "tampered-body" => report.observation.payload_sha256 = digest(b"unrelated-canonical-body"),
            _ => report.observation.row_ordinal = 99,
        }
        request.validate().unwrap();
        assert!(runtime.fixture.outbox.reserve("saved-source", &request).await.is_err(), "{defect}");
        assert_eq!(runtime.fixture.outbox.status(100, None).await.unwrap().entries.len(), entries);
        assert_eq!(runtime.native_calls(), before, "{defect}");
    }
    let mut encoded = serde_json::to_value(intent.selection().unwrap()).unwrap();
    encoded["focus"].as_object_mut().unwrap().remove("prerequisite");
    assert!(serde_json::from_value::<CollectionSelection>(encoded).is_err());
    assert_eq!(runtime.native_calls(), before);
}
