use super::*;
use culprit_collector::analytical::test_support::{GrantOutputCase, grant_output_evidence};
use culprit_engine::LocalCaptureAnalysisKind;
use serde_json::Value;

#[tokio::test]
async fn original_grants_reach_real_mcp_with_one_shared_request_decision_and_independent_raw_wait_context() {
    for case in GrantOutputCase::ALL {
        let (source, records) = grant_output_evidence(case);
        let native = source.observation.snapshot().unwrap().clone();
        let mut fixture = LocalCaptureFixture::snapshot(source, records.clone()).await;
        let manifest_before = std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap();
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::RequestMemory).await;
        assert!(!error, "{case:?}: {value}");
        assert_eq!(value["native_rows"], records.len());
        assert_eq!(value["observed_rows"], records.len());
        assert_eq!(value["read_complete"], true);
        assert_eq!(value["original_read_bounds"]["from"], native.observed_from);
        assert_eq!(value["original_read_bounds"]["until"], native.observed_until);
        let entries = value["request_memory"]["entries"].as_array().unwrap();
        assert_eq!(entries.len(), records.len());
        let holder = entries
            .iter()
            .find(|entry| entry["native_observation"]["request_wait"]["native_type"] == "ASYNC_NETWORK_IO")
            .unwrap();
        assert_eq!(holder["native_observation"]["observed_at"], "2020-01-01T12:00:00.000000000Z");
        assert_eq!(holder["native_observation"]["request_wait"]["elapsed_ms"], "3000");
        assert_eq!(holder["native_observation"]["granted_kibibytes"], "9007199254740993");
        let qualified = !matches!(
            case,
            GrantOutputCase::NegativeMarsPeer
                | GrantOutputCase::UnknownOwner
                | GrantOutputCase::MissingStatementIdentity
        );
        assert_eq!(holder["qualified_request"].is_object(), qualified, "{case:?}");
        assert_eq!(holder["participant_context"].is_object(), qualified, "{case:?}");
        assert_eq!(
            holder["mechanism"],
            if qualified { json!("reservation-during-client-output-wait") } else { Value::Null }
        );
        if qualified {
            assert_eq!(holder["participant_context"]["actor"], holder["qualified_request"]);
            assert_eq!(holder["participant_context"]["observed_at"], holder["native_observation"]["observed_at"]);
            if case == GrantOutputCase::SqlUnavailable {
                assert!(holder["participant_context"]["statement"].is_null());
            } else {
                assert_eq!(holder["participant_context"]["statement"]["availability"], "available");
                assert!(
                    holder["participant_context"]["statement"]["preview"].as_str().unwrap().contains("SELECT payload")
                );
            }
        }
        if case == GrantOutputCase::SharedLoginEstablished {
            let shared: Vec<_> =
                entries.iter().filter(|entry| entry["qualified_request"]["session"]["id"] == "61").collect();
            assert_eq!(shared.len(), 2);
            assert_ne!(shared[0]["qualified_request"]["request"], shared[1]["qualified_request"]["request"]);
        }
        if case == GrantOutputCase::NegativeMarsPeer {
            let waiter = entries.iter().find(|entry| entry["native_observation"]["state"] == "waiting").unwrap();
            assert_eq!(
                waiter["qualified_request"]["session"]["id"], "62",
                "only the recorded negative generation is refused"
            );
            assert_eq!(entries.iter().filter(|entry| entry["participant_context"].is_null()).count(), 2);
        }
        assert!(!value.to_string().contains("9264"));
        assert!(!value.to_string().contains("manifest_sha256"));
        assert_eq!(std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap(), manifest_before);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn tampered_original_wait_or_request_generation_cannot_be_reinterpreted_by_mcp_or_repaired_from_context() {
    for (before, after) in [("ASYNC_NETWORK_IO", "RESOURCE_SEMAPHORE"), ("request-1", "tampered-request-generation")] {
        let (source, records) = grant_output_evidence(GrantOutputCase::Qualified);
        let mut fixture = LocalCaptureFixture::snapshot(source, records).await;
        let file = fixture.capture_directory.join(&fixture.manifest.files[0].file);
        let original = std::fs::read_to_string(&file).unwrap();
        let tampered = original.replace(before, after);
        assert_ne!(original, tampered);
        std::fs::write(file, tampered).unwrap();
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::RequestMemory).await;
        assert!(error, "{before}: {value}");
        assert!(value.get("request_memory").is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
