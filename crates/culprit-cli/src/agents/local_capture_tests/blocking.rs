use super::*;
use culprit_collector::analytical::test_support::{BlockingCase, blocking_evidence, blocking_evidence_cases};
use culprit_engine::LocalCaptureAnalysisKind;

fn parent(fixture: &LocalCaptureFixture) -> DiagnosticCaptureRef {
    let SourceObservation::FocusedBlocking { result, .. } = &fixture.manifest.description.as_ref().unwrap().observation
    else {
        panic!()
    };
    result.focus.parent.capture_ref()
}
fn original_bytes(fixture: &LocalCaptureFixture) -> u64 {
    let directory = fixture.capture_directory.parent().unwrap().join(parent(fixture).capture.observation_id);
    let manifest = std::fs::read(directory.join("manifest.json")).unwrap();
    let parsed: CollectionManifest = serde_json::from_slice(&manifest).unwrap();
    manifest.len() as u64 + parsed.files.iter().map(|file| u64::from(file.bytes)).sum::<u64>()
}

#[tokio::test]
async fn graph_participants_keep_later_client_context_separate_from_the_exact_original() {
    for case in
        [BlockingCase::Matched, BlockingCase::IdleOwner, BlockingCase::HiddenOwner, BlockingCase::UnverifiedOriginal]
    {
        let (source, mut rows) = blocking_evidence(case);
        for record in &mut rows {
            let TelemetryRecord::Activity(row) = record else { panic!() };
            row.client = Some(ActivityClient {
                application: Some(format!("later-application-{}", row.key.session.id)),
                user: Some(format!("later-login-{}", row.key.session.id)),
                host: Some("later-client-host".into()),
            });
        }
        let mut fixture = LocalCaptureFixture::activity(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Blocking).await;
        assert!(!error, "{case:?}: {value}");
        let original = &value["blocking"]["original_participant_context"];
        assert!(original["client"].is_null(), "later clients never populate the original incident");
        for chain in value["blocking"]["chains"].as_array().unwrap() {
            for node in chain["nodes"].as_array().unwrap() {
                if node["context_available"] == false {
                    assert!(node["participant_context"].is_null());
                } else {
                    let context = &node["participant_context"];
                    let id = context["actor"]["session"]["id"].as_str().unwrap();
                    assert_eq!(context["client"]["user"], format!("later-login-{id}"));
                    assert_eq!(context["observed_at"], value["followup"]["native_observed_at"]);
                    assert_ne!(context["observed_at"], original["observed_at"]);
                    if context["idle"] == true {
                        assert!(context["statement"].is_null());
                    }
                }
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn genuine_two_capture_blocking_analysis_retains_exact_original_binding_and_native_gaps_without_live_work() {
    for (source, rows) in blocking_evidence_cases() {
        let original_focus = source.observation.focus().unwrap().clone();
        let current_rows = rows.clone();
        let mut fixture = LocalCaptureFixture::activity(source, rows).await;
        let SourceObservation::FocusedBlocking { snapshot, result } =
            &fixture.manifest.description.as_ref().unwrap().observation
        else {
            panic!()
        };
        let binding = result.binding;
        let membership = result.membership;
        let request_end = result.request_end;
        let state = snapshot.coverage.state;
        let count = snapshot.observations;
        let expected_bytes = original_bytes(&fixture) + fixture.original_bytes();
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) = culprit_core::i18n::scope_async(
            "fr",
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Blocking),
        )
        .await;
        assert!(!error, "{binding:?}/{membership:?}: {value}");
        let view = &value["blocking"];
        assert_eq!(view["title"], culprit_core::i18n::scope("en", || culprit_core::t!("view.blocking.title")));
        assert_eq!(view["binding"], json!(binding));
        assert_eq!(view["membership"], json!(membership));
        assert_eq!(view["request_end"], json!(request_end));
        assert_eq!(view["source_state"], json!(state));
        assert_eq!(view["observed_rows"], count);
        assert_eq!(view["read_complete"], true);
        assert_eq!(view["original_authenticated"], true);
        assert_eq!(view["historical_context"], true);
        let original_participant = &view["original_participant_context"];
        assert_eq!(original_participant["actor"], json!(original_focus.original.key));
        assert_eq!(original_participant["observed_at"], original_focus.original_observed_at);
        assert_eq!(original_participant["client"], json!(original_focus.original.client));
        for chain in view["chains"].as_array().unwrap() {
            for node in chain["nodes"].as_array().unwrap() {
                let context = &node["participant_context"];
                if node["context_available"] == false {
                    assert!(context.is_null());
                    continue;
                }
                let actor: ActivityKey = serde_json::from_value(context["actor"].clone()).unwrap();
                let row = current_rows
                    .iter()
                    .find_map(|record| match record {
                        TelemetryRecord::Activity(row) if row.key == actor => Some(row),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(context["client"], json!(row.client));
                assert_eq!(context["command"], json!(row.command));
                assert_eq!(context["database"], json!(row.database));
                assert_eq!(context["observed_at"], value["followup"]["native_observed_at"]);
                assert_ne!(context["observed_at"], original_participant["observed_at"]);
                if row.key.request.is_none() {
                    assert!(context["statement"].is_null());
                }
            }
        }
        assert_eq!(value["original"]["retained_rows"], 1);
        assert_eq!(value["followup"]["retained_rows"], count);
        assert_ne!(value["original"]["native_observed_at"], value["followup"]["native_observed_at"]);
        assert_ne!(value["original"]["reference"], value["followup"]["reference"]);
        assert_eq!(value["local_verification_bytes"], expected_bytes);
        assert_eq!(value["followup"]["collected_from"], fixture.manifest.started_at);
        assert_eq!(value["followup"]["collected_until"], fixture.manifest.finished_at.as_deref().unwrap());
        if binding == FocusedRequestBinding::UnverifiedOriginalScope {
            assert_eq!(value["original"]["qualified_local_owner"], false);
            assert_eq!(value["followup"]["qualified_local_owner"], true);
        }
        if state == SourceState::Unavailable {
            assert_eq!(view["membership"], json!(FocusedRequestMembership::Unavailable));
            assert!(view["chains"].as_array().unwrap().is_empty());
            assert_eq!(value["followup"]["native_error_code"], json!(culprit_core::ErrorCode::PermissionDenied));
        }
        for private in [
            "manifest_sha256",
            "source_scope_id",
            "payload_sha256",
            "statement_identity",
            "private-blocking-native",
            "fixture.invalid",
            "action_target",
        ] {
            assert!(!value.to_string().contains(private), "{private}");
        }
        assert!(value.to_string().len() <= 64 * 1024);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let (error, repeat) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Blocking).await;
        assert!(!error);
        assert_eq!(value, repeat);
    }
}

#[tokio::test]
async fn blocking_presentation_limits_graph_details_without_truncating_original_counts_or_borrowing_sql() {
    let (mut source, mut rows) = blocking_evidence(BlockingCase::Matched);
    let owner = rows.pop().unwrap();
    let TelemetryRecord::Activity(template) = owner else { panic!() };
    let TelemetryRecord::Activity(waiter) = &mut rows[0] else { panic!() };
    waiter.wait.as_mut().unwrap().owners = (50..57)
        .map(|id| ActivityWaitOwner::Session {
            id: id.to_string(),
            generation: Some(format!("2020-01-01T10:00:{id}Z")),
        })
        .collect();
    for id in 50..57 {
        let mut owner = template.as_ref().clone();
        owner.key.session =
            NativeActivityIdentity { id: id.to_string(), generation: format!("2020-01-01T10:00:{id}Z") };
        owner.statement_identity = Some(NativeIdentity::new(format!("native-statement-{id}")));
        owner.statement = DefinitionPayload::sql(
            &format!(
                "SELECT {}, 'private-literal-canary' FROM orders WHERE tenant_id=9264",
                (0..100).map(|index| format!("column_{index}")).collect::<Vec<_>>().join(",")
            ),
            Engine::Postgres,
        );
        rows.push(owner.into());
    }
    let SourceObservation::FocusedBlocking { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.observations = rows.len() as u32;
    snapshot.native_rows = snapshot.observations;
    snapshot.coverage.records = snapshot.observations;
    let mut fixture = LocalCaptureFixture::activity(source, rows).await;
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, calls.clone());
    let (error, value) =
        analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Blocking).await;
    assert!(!error, "{value}");
    let view = &value["blocking"];
    assert_eq!(view["observed_rows"], 8);
    assert_eq!(value["followup"]["retained_rows"], 8);
    assert_eq!(view["relationships_available"], 7);
    assert_eq!(view["relationships_presented"], 5);
    assert_eq!(view["relationships_omitted"], 2);
    assert_eq!(view["chains_available"], 7);
    assert_eq!(view["chains_presented"], 5);
    assert_eq!(view["chains_omitted"], 2);
    assert_eq!(view["read_complete"], true);
    for chain in view["chains"].as_array().unwrap() {
        let node = &chain["nodes"][0];
        assert_eq!(node["sql_preview"].as_str().unwrap().chars().count(), 512);
        assert_eq!(node["sql_preview_truncated"], true);
        assert_eq!(node["owner"]["generation"], format!("2020-01-01T10:00:{}Z", node["session"].as_str().unwrap()));
        assert_eq!(chain["nodes_available"], 2);
        assert_eq!(chain["nodes_omitted"], 0);
    }
    assert!(!value.to_string().contains("private-literal-canary"));
    assert!(!value.to_string().contains("9264"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn idle_or_prepared_owners_never_gain_previous_sql_or_a_current_administrative_action() {
    for case in [BlockingCase::IdleOwner, BlockingCase::PreparedOwner, BlockingCase::HiddenOwner] {
        let (source, mut rows) = blocking_evidence(case);
        if case == BlockingCase::IdleOwner {
            let TelemetryRecord::Activity(owner) = rows.last_mut().unwrap() else { panic!() };
            assert!(owner.idle && owner.key.request.is_none());
            owner.statement = DefinitionPayload::sql(
                "SELECT private_prior_batch_column FROM orders WHERE tenant_id=9264",
                Engine::Postgres,
            );
        }
        let mut fixture = LocalCaptureFixture::activity(source, rows).await;
        drop(fixture.outbox.take());
        let server = server(&fixture, Arc::new(AtomicUsize::new(0)));
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Blocking).await;
        assert!(!error, "{case:?}: {value}");
        let node = &value["blocking"]["chains"][0]["nodes"][0];
        assert!(node["sql_preview"].is_null());
        assert!(node["participant_context"]["statement"].is_null());
        assert!(!value.to_string().contains("private_prior_batch_column"));
        assert!(node.get("action_target").is_none());
        assert!(node.get("application").is_none() && node.get("host").is_none());
        if case != BlockingCase::IdleOwner {
            assert_eq!(node["context_available"], false);
        }
        if case == BlockingCase::PreparedOwner {
            assert_eq!(node["owner"]["kind"], "transaction");
            assert_eq!(node["owner"]["native_id"], "0");
            assert!(
                value["blocking"]["limitations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|text| text.as_str().unwrap().contains("prepared"))
            );
        }
    }
}

#[tokio::test]
async fn a_long_chain_keeps_native_relationship_counts_and_reports_omitted_nodes() {
    let (mut source, mut rows) = blocking_evidence(BlockingCase::Matched);
    let TelemetryRecord::Activity(template) = rows.pop().unwrap() else { panic!() };
    let TelemetryRecord::Activity(waiter) = &rows[0] else { panic!() };
    let wait = waiter.wait.as_ref().unwrap().clone();
    for id in 43..=48 {
        let mut owner = template.as_ref().clone();
        owner.key.session =
            NativeActivityIdentity { id: id.to_string(), generation: format!("2020-01-01T11:00:{id}Z") };
        owner.statement_identity = Some(NativeIdentity::new(format!("native-pg-statement-{id}")));
        owner.waiting_on_lock = Some(id < 48);
        owner.wait = if id < 48 {
            let mut next = wait.clone();
            next.owners = vec![ActivityWaitOwner::Session {
                id: (id + 1).to_string(),
                generation: Some(format!("2020-01-01T11:00:{}Z", id + 1)),
            }];
            Some(next)
        } else {
            None
        };
        rows.push(owner.into());
    }
    let SourceObservation::FocusedBlocking { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.observations = rows.len() as u32;
    snapshot.native_rows = snapshot.observations;
    snapshot.coverage.records = snapshot.observations;
    let mut fixture = LocalCaptureFixture::activity(source, rows).await;
    drop(fixture.outbox.take());
    let server = server(&fixture, Arc::new(AtomicUsize::new(0)));
    let (error, value) =
        analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Blocking).await;
    assert!(!error, "{value}");
    let report = &value["blocking"];
    assert_eq!(report["observed_rows"], 7);
    assert_eq!(report["relationships_available"], 6);
    assert_eq!(report["chains_available"], 1);
    assert_eq!(report["chains_omitted"], 0);
    let chain = &report["chains"][0];
    assert_eq!(chain["waiters"], 6);
    assert_eq!(chain["nodes_available"], 7);
    assert_eq!(chain["nodes_presented"], 5);
    assert_eq!(chain["nodes_omitted"], 2);
    let nodes = chain["nodes"].as_array().unwrap();
    assert!(nodes[0]["parent"].is_null());
    for pair in nodes.windows(2) {
        assert_eq!(pair[1]["parent"], pair[0]["reference"]);
    }
    assert_eq!(report["read_complete"], true);
}

#[tokio::test]
async fn a_missing_original_cannot_return_a_cached_graph_or_open_a_source() {
    let (source, rows) = blocking_evidence(BlockingCase::Matched);
    let mut fixture = LocalCaptureFixture::activity(source, rows).await;
    let original = fixture.capture_directory.parent().unwrap().join(parent(&fixture).capture.observation_id);
    std::fs::remove_file(original.join("manifest.json")).unwrap();
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, calls.clone());
    let (error, value) =
        analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Blocking).await;
    assert!(error);
    assert!(value.get("blocking").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
