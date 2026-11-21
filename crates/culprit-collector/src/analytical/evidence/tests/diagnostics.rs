use super::*;
use crate::delivery::evidence::RowReceipt;
use serde_json::{Map, Value, json};

#[tokio::test]
async fn diagnostic_warehouse_details_restore_only_authenticated_scopes_and_match_exact_native_receipts() {
    let catalog = LogicalCatalog::new().unwrap();
    for (source, records) in test_support::diagnostic_evidence_cases() {
        let recovery =
            test_support::Recovery::capture("fixture:test", catalog.digest(), source.clone(), records.clone(), 230_000);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let batch =
            range.manifest().batches.iter().find(|batch| batch.stream == TelemetryStream::OperationalSamples).unwrap();
        let original = ExportedRecord::new(&source, &range.manifest().capture, records[0].clone()).unwrap();
        let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(1, &original).unwrap();
        let schema = catalog.get(batch.stream).unwrap();
        let values: Map<String, Value> = schema
            .fields()
            .iter()
            .zip(schema.project(&input).unwrap())
            .map(|(field, value)| (field.name.into(), value))
            .collect();
        assert!(values["Details"]["episode"]["scope"].get("target").is_none());
        assert_eq!(values["Details"]["episode"]["scope"].as_object().unwrap().len(), 1);
        assert!(!serde_json::to_string(&input).unwrap().contains("private-diagnostic"));
        let decode::Decoded::Record(decoded) = decode::record(&range, batch.stream, &values, &batch.rows[0]).unwrap()
        else {
            panic!("the exact target is reconstructed from authenticated private recovery authority")
        };
        assert_eq!(*decoded, original);
        assert_eq!(RowReceipt::from_export(&decoded, 1).unwrap(), batch.rows[0]);
        for target in [serde_json::to_value(&source.target).unwrap(), json!({"kind":"database","database":"forged"})] {
            let mut injected = values.clone();
            injected.get_mut("Details").unwrap()["episode"]["scope"]["target"] = target;
            assert!(
                decode::record(&range, batch.stream, &injected, &batch.rows[0]).is_err(),
                "warehouse target injection is never trusted or silently overridden"
            );
        }
    }
}

#[test]
fn missing_unrelated_or_wrong_private_scope_bindings_reject_revisions_and_checkpoint_admission() {
    let capture = test_support::fixture().0.capture;
    for (source, records) in test_support::diagnostic_evidence_cases() {
        for shape in ["missing", "unrelated", "wrong-value"] {
            let mut broken = source.clone();
            let SourceObservation::Diagnostics(description) = &mut broken.observation else { panic!() };
            let (key, scope) = description.scope_bindings.first_key_value().unwrap();
            let key = key.clone();
            let mut scope = scope.clone();
            scope.source = "unrelated-native-source".into();
            description.scope_bindings = match shape {
                "missing" => Default::default(),
                "unrelated" => [(scope.key().unwrap(), scope)].into(),
                "wrong-value" => [(key, scope)].into(),
                _ => unreachable!(),
            };
            for record in &records {
                assert!(
                    ExportedRecord::new(&broken, &capture, record.clone()).is_err(),
                    "{shape}/{:?}",
                    record.stream()
                );
            }
        }
    }
}

#[tokio::test]
async fn source_read_answers_publish_only_closed_consistency_metadata_and_require_the_original_payload() {
    let catalog = LogicalCatalog::new().unwrap();
    for case in [
        test_support::IncidentHistoryCase::TemporaryResources,
        test_support::IncidentHistoryCase::TemporaryResourcesPartial,
    ] {
        let (source, records) = test_support::incident_history_evidence(case);
        let recovery =
            test_support::Recovery::capture("fixture:test", catalog.digest(), source.clone(), records.clone(), 230_001);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let batch =
            range.manifest().batches.iter().find(|batch| batch.stream == TelemetryStream::OperationalSamples).unwrap();
        let original = ExportedRecord::new(&source, &range.manifest().capture, records[0].clone()).unwrap();
        let original_bytes = serde_json::to_vec(&original).unwrap();
        let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(1, &original).unwrap();
        let public = &input["record"]["data"]["fact"]["data"]["episode"]["source_read"];
        let keys = public.as_object().unwrap().keys().map(String::as_str).collect::<std::collections::BTreeSet<_>>();
        assert_eq!(keys, ["question", "focus_key", "state", "result", "content_sha256"].into_iter().collect());
        assert_eq!(public["state"], "answered");
        assert_eq!(public["question"], "temporary-space-contributors");
        assert!(valid_digest(public["content_sha256"].as_str().unwrap()));
        let schema = catalog.get(batch.stream).unwrap();
        let values: Map<String, Value> = schema
            .fields()
            .iter()
            .zip(schema.project(&input).unwrap())
            .map(|(field, value)| (field.name.into(), value))
            .collect();
        assert!(matches!(
            decode::record(&range, batch.stream, &values, &batch.rows[0]).unwrap(),
            decode::Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired)
        ));
        assert_eq!(serde_json::to_vec(&original).unwrap(), original_bytes);
        let restored: ExportedRecord = serde_json::from_slice(&original_bytes).unwrap();
        assert_eq!(restored, original);
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::DiagnosticRevision(revision),
            ..
        }) = &restored.record
        else {
            panic!()
        };
        let SourceReadStageState::Answered { answer } = &revision.episode.source_read.as_ref().unwrap().state else {
            panic!()
        };
        let SourceReadAnswer::TemporaryResources(answer) = answer.as_ref() else {
            panic!("expected temporary-resource answer")
        };
        assert!(answer.basis_context.has_qualified_local_owner());
        assert!(answer.result_context.has_qualified_local_owner());
        assert_ne!(answer.basis_context.observed_until, answer.result_context.observed_from);
        // Both destination conformance readers separately exercise the real authenticated payload fallback.
        for key in ["focus_key", "content_sha256"] {
            let mut changed = values.clone();
            changed.get_mut("Details").unwrap()["episode"]["source_read"][key] = json!(digest(b"forged public key"));
            assert!(matches!(
                decode::record(&range, batch.stream, &changed, &batch.rows[0]).unwrap(),
                decode::Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired)
            ));
        }
    }
}

#[test]
fn every_existing_pointer_stage_is_curated_without_publishing_its_original_target_or_scope() {
    let fixture = test_support::compiler_admission_investigation_fixture();
    let source = fixture.baseline_source;
    let mut record = fixture.baseline_record.record;
    let TelemetryRecord::Activity(actor) = &mut record else { panic!() };
    let wait = actor.wait.as_mut().unwrap();
    wait.class = Some(ActivityWaitClass::LogRateGovernance);
    wait.native_type = Some("LOG_RATE_GOVERNOR".into());
    let capture = CaptureIdentity {
        observation_id: "original-log-wait".into(),
        revision: Some(CaptureRevision {
            outbox_id: "public-stage-fixture".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let original = ExportedRecord::new(&source, &capture, record).unwrap();
    let reference = DiagnosticEvidenceRef::from_export(&source, &digest(b"log-wait-manifest"), &original, 0).unwrap();
    let focus = SourceReadFocus {
        basis: Box::new(reference),
        target: source.target,
        question: SourceReadQuestion::HyperscaleLogComponents,
        prerequisite: SourceReadPrerequisite::ObservedLogRateWait,
    };
    let result = DiagnosticCaptureRef {
        capture: CaptureIdentity {
            observation_id: "native-log-context".into(),
            revision: Some(CaptureRevision {
                outbox_id: "public-stage-fixture".into(),
                owner_epoch: "1".into(),
                sequence: "2".into(),
            }),
        },
        manifest_sha256: digest(b"log-context-manifest"),
        source_scope_id: digest(b"log-context-scope"),
    };
    for state in [
        SourceReadStageState::Pending,
        SourceReadStageState::Deferred { issue: DiagnosticAnalysisIssue::CostUnqualified },
        SourceReadStageState::Collecting,
        SourceReadStageState::Captured { capture: result.clone() },
        SourceReadStageState::Stopped { issue: DiagnosticAnalysisIssue::Interrupted },
    ] {
        let stage = SourceReadStage { focus: focus.clone(), state };
        let raw = serde_json::to_vec(&stage).unwrap();
        let public = crate::analytical::investigations::source_stage(&stage).unwrap();
        assert_eq!(public.as_object().unwrap().len(), 5);
        assert_eq!(public["focus_key"], focus.key().unwrap());
        assert_eq!(public["content_sha256"], digest(&raw));
        assert!(
            serde_json::from_value::<SourceReadStage>(public).is_err(),
            "public consistency metadata is not source authority"
        );
        assert_eq!(serde_json::to_vec(&stage).unwrap(), raw);
    }
}
