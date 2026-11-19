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
        let references = original_entry_references(&fixture, &manifest_before, &records);
        let content = content_after_reference_verification(&value, &references)
            .expect("every MCP entry must retain its exact original manifest/scope/observation/native-key reference");
        assert!(!content.contains("9264"), "{case:?}");
        assert!(!value.to_string().contains("manifest_sha256"));
        assert_eq!(std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap(), manifest_before);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

fn original_entry_references(
    fixture: &LocalCaptureFixture,
    manifest_bytes: &[u8],
    records: &[TelemetryRecord],
) -> Vec<String> {
    let source = fixture.manifest.description.as_ref().unwrap();
    let context = NativeReadContext::from_snapshot(source).unwrap();
    let observations = records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::ExecutionGrant(grant),
                ..
            }) = record
            else {
                panic!("the original fixture contains only native execution grants")
            };
            grant.as_ref()
        })
        .collect::<Vec<_>>();
    let report = culprit_checks::request_memory::explain(
        culprit_checks::request_memory::RequestMemoryEvidence { context: &context, observations: &observations },
        None,
    )
    .unwrap();
    let manifest = digest(manifest_bytes);
    let scope = source.captured_range().unwrap().scope_id;
    let observation = &fixture.manifest.capture.observation_id;
    // An independent oracle for the public entry-reference contract; ordering comes from the shared report owner.
    report
        .entries
        .iter()
        .map(|entry| {
            digest(
                format!("local-snapshot-entry:{manifest}:{scope}:{observation}:{}", entry.observation_key).as_bytes(),
            )
        })
        .collect()
}

fn content_after_reference_verification(value: &Value, expected: &[String]) -> Option<String> {
    let mut content = value.clone();
    let entries = content.pointer_mut("/request_memory/entries")?.as_array_mut()?;
    if entries.len() != expected.len() {
        return None;
    }
    for (entry, expected) in entries.iter_mut().zip(expected) {
        if entry.get("reference")?.as_str()? != expected {
            return None;
        }
        // Only these exact authenticated slots are opaque identities. All other keys, text and numbers stay scanned.
        entry.as_object_mut()?.remove("reference")?;
    }
    Some(content.to_string())
}

fn privacy_value(reference: &str) -> Value {
    json!({
        "request_memory":{"entries":[{
            "reference":reference,"explanation":"native reservation",
            "participant_context":{"statement":{"preview":"SELECT payload FROM dbo.items WHERE id = ?"}}
        }]},
        "native_rows":1,"unrelated_number":0
    })
}

#[test]
fn a_verified_reference_can_contain_the_canary_but_sql_prose_numbers_and_other_keys_cannot() {
    let reference = digest(b"local-snapshot-entry:privacy-control:4937");
    assert_eq!(reference, "fe51cd1b310885c9a05dce57e09264304f5813529e3ffc24ca4028b1c7a27bef");
    let value = privacy_value(&reference);
    let expected = [reference.clone()];
    assert!(!content_after_reference_verification(&value, &expected).unwrap().contains("9264"));
    for (pointer, leaked) in [
        ("/request_memory/entries/0/explanation", json!("Customer 9264")),
        ("/request_memory/entries/0/explanation", json!(reference)),
        ("/request_memory/entries/0/participant_context/statement/preview", json!("SELECT payload WHERE id = 9264")),
        ("/unrelated_number", json!(9264)),
    ] {
        let mut changed = value.clone();
        *changed.pointer_mut(pointer).unwrap() = leaked;
        assert!(content_after_reference_verification(&changed, &expected).unwrap().contains("9264"), "{pointer}");
    }
    let mut changed = value;
    changed["9264"] = json!("an unrelated key remains content");
    assert!(content_after_reference_verification(&changed, &expected).unwrap().contains("9264"));
}

#[test]
fn foreign_missing_repeated_or_swapped_references_cannot_bypass_the_content_guard() {
    let reference = digest(b"local-snapshot-entry:privacy-control:4937");
    let value = privacy_value(&reference);
    let expected = [reference.clone()];
    for foreign in [json!(digest(b"another native key")), json!("SELECT 9264"), json!(9264), Value::Null] {
        let mut changed = value.clone();
        changed["request_memory"]["entries"][0]["reference"] = foreign;
        assert!(content_after_reference_verification(&changed, &expected).is_none());
    }
    let mut missing = value.clone();
    missing["request_memory"]["entries"][0].as_object_mut().unwrap().remove("reference");
    assert!(content_after_reference_verification(&missing, &expected).is_none());
    let second = digest(b"second native key");
    let expected = [reference, second.clone()];
    let mut pair = value.clone();
    pair["request_memory"]["entries"].as_array_mut().unwrap().push(value["request_memory"]["entries"][0].clone());
    assert!(content_after_reference_verification(&pair, &expected).is_none(), "a duplicate cannot replace its peer");
    pair["request_memory"]["entries"][1]["reference"] = json!(second);
    assert!(content_after_reference_verification(&pair, &expected).is_some());
    pair["request_memory"]["entries"].as_array_mut().unwrap().swap(0, 1);
    assert!(content_after_reference_verification(&pair, &expected).is_none(), "each reference keeps its native owner");
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
