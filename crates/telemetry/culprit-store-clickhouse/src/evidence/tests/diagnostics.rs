use super::*;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn diagnostic_revisions_keep_private_targets_out_of_warehouse_rows_and_restore_exact_originals() {
    for (source, records) in test_support::diagnostic_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let encoded = serde_json::to_string(&fixture.rows).unwrap();
        assert!(!encoded.contains("private-diagnostic") && !encoded.contains("/private/diagnostics"));
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let details = schema.columns().iter().position(|column| column.name == "Details").unwrap();
        let native: Value = serde_json::from_str(fixture.rows[0][details].as_str().unwrap()).unwrap();
        assert!(native["episode"]["scope"].get("target").is_none());
        fixture.replies(vec![fixture.rows.clone()]);
        let captured = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        test_support::assert_diagnostic_capture(&captured, &source, &records);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));

        assert_eq!(native["episode"]["scope"].as_object().unwrap().len(), 1);
        assert!(native["episode"]["scope"]["key"].is_string());
        let published = serde_json::to_string(&fixture.publication).unwrap();
        assert!(!published.contains("private-diagnostic"));
        for field in ["target", "partition", "key"] {
            let mut injected = fixture.rows[0].clone();
            let mut changed = native.clone();
            changed["episode"]["scope"][field] = match field {
                "target" => serde_json::to_value(&source.target).unwrap(),
                "partition" => json!({"kind":"instance-local","member":null,"epoch":null}),
                "key" => json!(digest(b"unbound native scope")),
                _ => unreachable!(),
            };
            injected[details] = json!(serde_json::to_string(&changed).unwrap());
            fixture.replies(vec![vec![injected]]);
            let error = fixture
                .reader
                .read_capture(
                    &fixture.range,
                    &[EvidenceFamily::OperationalSamples],
                    EvidenceReadBudget::default(),
                    None,
                )
                .await
                .unwrap_err();
            assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{field}");
            assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        }
    }
}

#[tokio::test]
async fn shared_investigation_provenance_stays_private_and_requires_explicit_native_recovery() {
    let (source, records) = test_support::shared_investigation_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    assert!(!serde_json::to_string(&fixture.rows).unwrap().contains("private-diagnostic"));
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    fixture.replies(vec![fixture.rows.clone()]);
    let preview = fixture
        .reader
        .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
        .await
        .unwrap();
    assert!(!preview.is_read_complete());
    assert!(preview.records().is_empty());
    assert!(preview.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::NativeRepresentationRequired));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    fixture.replies(vec![fixture.rows.clone()]);
    let restored = fixture
        .reader
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::OperationalSamples],
            EvidenceReadBudget {
                recovery_payloads: Some(RecoveryPayloadBudget::default()),
                ..EvidenceReadBudget::default()
            },
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap();
    test_support::assert_diagnostic_capture(&restored, &source, &records);
    assert_eq!(restored.metrics().recovery_objects, 1);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before + 1);
    let TelemetryRecord::Operational(row) = &restored.records()[0].row().record else { panic!() };
    let OperationalFact::DiagnosticRevision(revision) = &row.fact else { panic!() };
    let event = revision.investigation.as_ref().unwrap();
    let DiagnosticExecutionBinding::Shared { owner } = &event.intent.execution else { panic!() };
    assert!(revision.inputs.contains(owner));
    assert!(revision.episode.scope.target.database().is_some());
    assert!(matches!(event.intent.subject(), CollectionSubject::Shared { .. }));
}

#[tokio::test]
async fn selected_incident_revisions_survive_recovery_even_when_resolved_episodes_leave_working_set() {
    for case in test_support::IncidentHistoryCase::ALL {
        let (source, rows) = test_support::incident_history_evidence(*case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        assert!(!serde_json::to_string(&fixture.rows).unwrap().contains("private-diagnostic"));
        fixture.replies(vec![fixture.rows.clone()]);
        let captured = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        let captured = if captured.is_read_complete() {
            captured
        } else {
            assert!(captured.records().is_empty());
            assert!(captured.gaps().iter().all(|gap| gap.reason == EvidenceGapReason::NativeRepresentationRequired));
            *fixture.recovery.fault.lock().unwrap() = Fault::None;
            fixture.replies(vec![fixture.rows.clone()]);
            let restored = fixture
                .reader
                .read_capture(
                    &fixture.range,
                    &[EvidenceFamily::OperationalSamples],
                    EvidenceReadBudget {
                        recovery_payloads: Some(RecoveryPayloadBudget::default()),
                        ..EvidenceReadBudget::default()
                    },
                    Some(fixture.recovery.as_ref()),
                )
                .await
                .unwrap();
            assert_eq!(restored.metrics().recovery_objects, 1);
            restored
        };
        test_support::assert_incident_history_capture(&captured, &source, &rows);
    }
}

#[tokio::test]
async fn automatic_query_compilation_answers_require_original_recovery_and_preserve_context() {
    for case in test_support::QueryCompilationFollowupCase::ALL {
        let (source, records) = test_support::query_compilation_followup_evidence(*case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.replies(vec![fixture.rows.clone()]);
        let preview = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        assert!(!preview.is_read_complete());
        assert!(preview.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::NativeRepresentationRequired));
        *fixture.recovery.fault.lock().unwrap() = Fault::None;
        fixture.replies(vec![fixture.rows.clone()]);
        let restored = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::OperationalSamples],
                EvidenceReadBudget {
                    recovery_payloads: Some(RecoveryPayloadBudget::default()),
                    ..EvidenceReadBudget::default()
                },
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap();
        test_support::assert_query_compilation_followup_capture(&restored, &source, &records, *case);
        assert_eq!(restored.metrics().recovery_objects, 1);
    }
}
