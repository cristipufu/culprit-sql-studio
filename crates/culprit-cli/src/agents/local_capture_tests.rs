//! Startup composition reaches canonical saved originals without constructing a native provider or a warehouse.
mod blocking;
mod cdc;
mod change_feed;
mod full_text_populations;
mod grant_output;
mod hyperscale_log_components;
mod participant_context;
mod replication;
mod rowgroups_backup;
mod snapshots;
mod source_overhead;
mod table_memory;
mod tempdb_metadata_memory;
use super::*;
use clap::Parser;
use culprit_collector::{
    analytical::test_support::{LocalCaptureFixture, page_server_read_evidence_cases},
    outbox::CompletedCaptureBudget,
};
use culprit_core::{AuthKind, Engine, SourceState, telemetry::*};
use culprit_engine::{LocalCaptureAnalysisKind, NewConnection, OutboxCaptureReader, ProviderRegistration};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn local_originals_require_one_explicit_startup_directory_and_never_per_tool_source_configuration() {
    let cli = crate::Cli::try_parse_from([
        "culprit",
        "mcp",
        "--evidence-outbox",
        "owned-outbox",
        "--connection",
        "local-evidence",
    ])
    .unwrap();
    let crate::Command::Mcp(args) = cli.command else { panic!("MCP startup") };
    assert_eq!(args.evidence_outbox, Some(PathBuf::from("owned-outbox")));
    assert_eq!(args.connections, ["local-evidence"]);
    let cli = crate::Cli::try_parse_from(["culprit", "mcp"]).unwrap();
    let crate::Command::Mcp(args) = cli.command else { panic!("MCP startup") };
    assert!(args.evidence_outbox.is_none());
}

fn server(fixture: &LocalCaptureFixture, native_calls: Arc<AtomicUsize>) -> culprit_mcp::CulpritMcp {
    let workspace = Workspace::open_in(fixture.directory.parent().unwrap().join("workspace")).with_provider(
        Engine::Mssql,
        ProviderRegistration::new(
            Arc::new(move |_| {
                native_calls.fetch_add(1, Ordering::SeqCst);
                Err(Error::internal("offline analysis must not construct a native provider"))
            }),
            Vec::new(),
        )
        .unwrap(),
    );
    workspace
        .add_connection(NewConnection {
            name: "local-evidence".into(),
            input: "Server=localhost,14333;Database=fixture".into(),
            engine: Some(Engine::Mssql),
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap();
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    culprit_mcp::CulpritMcp::new(Arc::new(workspace), culprit_mcp::Options::default())
        .with_local_captures(Arc::new(reader))
}

async fn call(server: &culprit_mcp::CulpritMcp, id: &str) -> (bool, serde_json::Value) {
    analyze(server, id, culprit_engine::LocalCaptureAnalysisKind::PageServerReads).await
}
async fn analyze(
    server: &culprit_mcp::CulpritMcp,
    id: &str,
    analysis: culprit_engine::LocalCaptureAnalysisKind,
) -> (bool, serde_json::Value) {
    let result = server
        .handle(
            "local-capture-test",
            "analyze_capture",
            json!({
                "connection":"local-evidence", "observation_id":id, "analysis":analysis,
            })
            .as_object()
            .unwrap()
            .clone(),
        )
        .await;
    (result.is_error == Some(true), serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap())
}

fn manifest_digest(fixture: &LocalCaptureFixture) -> String {
    digest(&std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap())
}

#[tokio::test]
async fn genuine_ready_originals_reach_mcp_with_exact_counts_folded_definitions_and_no_private_authority() {
    for name in
        ["native_fragment", "empty", "columns_unavailable", "separate_fragments", "single_fragment_with_definitions"]
    {
        let mut case = page_server_read_evidence_cases().into_iter().find(|case| case.name == name).unwrap();
        let SourceObservation::QueryHistory(history) = &mut case.source.observation else { panic!() };
        history.native_properties.insert("private-proof-canary".into(), "private-value-canary".into());
        let from = history.interval.from.clone();
        let until = history.interval.until.clone();
        if name == "native_fragment" {
            history.population_catalog.as_mut().unwrap().membership =
                culprit_core::RuntimePopulationMembership::ObservedWindow { from: from.clone(), until: until.clone() };
        }
        let membership = history.population_catalog.as_ref().map(|catalog| catalog.membership.clone());
        let mut fixture = LocalCaptureFixture::query_history(case.source, case.records).await;
        assert_eq!(fixture.manifest.status, CollectionStatus::Complete, "{name}");
        drop(fixture.outbox.take());
        let native_calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, native_calls.clone());
        let (error, result) =
            culprit_core::i18n::scope_async("fr", call(&server, &fixture.manifest.capture.observation_id)).await;
        assert!(!error, "{name}: {result}");
        let view = &result["page_server_reads"];
        assert_eq!(view["title"], culprit_core::i18n::scope("en", || culprit_core::t!("view.page_server_reads.title")));
        let rows =
            fixture.manifest.files.iter().find(|file| file.stream == TelemetryStream::QueryRuntime).unwrap().rows;
        assert_eq!(view["selected_rows"], rows, "{name}");
        assert_eq!(view["observed_rows"], rows);
        assert_eq!(view["read_complete"], true);
        assert_eq!(view["reference_integrity_complete"], true);
        assert_eq!(view["references_complete"], fixture.manifest.references.complete);
        assert_eq!(result["native_history_window"], json!({"from":from,"until":until}));
        assert_eq!(result["runtime_population_membership"], json!(membership));
        assert_eq!(result["collected_from"], fixture.manifest.started_at);
        assert_eq!(result["collected_until"], fixture.manifest.finished_at.as_deref().unwrap());
        assert_eq!(result["local_verification_bytes"], fixture.original_bytes());
        assert_eq!(native_calls.load(Ordering::SeqCst), 0);
        if name == "native_fragment" {
            assert_eq!(view["entries"][0]["executions"], "9007199254740993");
            assert_eq!(view["entries"][0]["informational"], true);
            assert_eq!(view["partial"], true, "semantic gaps remain distinct from a fully read body");
            assert!(view["entries"][0]["sql_preview"].is_null());
        }
        if name == "empty" {
            assert_eq!(view["entries_presented"], 0);
        }
        if name == "single_fragment_with_definitions" {
            // The original capture supplies this definition; global namespace qualification stays independent.
            assert_eq!(view["entries"][0]["statement_binding"], "captured");
            assert!(view["entries"][0]["sql_preview"].as_str().unwrap().chars().count() <= 512);
        }
        let text = result.to_string();
        for private in [
            "private-proof-canary",
            "private-value-canary",
            "manifest_sha256",
            "source_sha256",
            "payload_sha256",
            "qualifiers",
            "fixture.invalid",
        ] {
            assert!(!text.contains(private), "{name}: {private}");
        }
        assert!(text.len() <= 64 * 1024);
        let (error, repeat) = call(&server, &fixture.manifest.capture.observation_id).await;
        assert!(!error);
        assert_eq!(result, repeat, "immutable original positions yield stable references");
    }
}

#[tokio::test]
async fn corrupted_originals_never_fall_back_to_cached_reports_or_open_a_native_source() {
    let case = page_server_read_evidence_cases().remove(0);
    let mut fixture = LocalCaptureFixture::query_history(case.source, case.records).await;
    let body = fixture.manifest.files.iter().find(|file| file.stream == TelemetryStream::QueryRuntime).unwrap();
    let path = fixture.capture_directory.join(&body.file);
    let original = std::fs::read_to_string(&path).unwrap();
    let forged = original.replace("9007199254740993", "9007199254740994");
    assert_ne!(original, forged);
    assert_eq!(original.len(), forged.len());
    std::fs::write(path, forged).unwrap();
    drop(fixture.outbox.take());
    let native_calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, native_calls.clone());
    let (error, result) = call(&server, &fixture.manifest.capture.observation_id).await;
    assert!(error);
    assert!(result.get("page_server_reads").is_none());
    assert_eq!(native_calls.load(Ordering::SeqCst), 0);
}

fn clerk_originals(groups: usize, state: SourceState) -> (CollectionDescription, Vec<TelemetryRecord>) {
    use culprit_collector::analytical::test_support::{MemoryClerkCase, memory_clerk_evidence};
    let (mut source, mut originals) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let unrelated = originals.pop().unwrap();
    let TelemetryRecord::Operational(original) = &originals[0] else { panic!() };
    let OperationalFact::CounterSet(template) = &original.fact else { panic!() };
    let mut rows = (0..groups)
        .map(|node| {
            let mut set = template.as_ref().clone();
            set.identity.qualifiers.insert("memory_node_id".into(), node.to_string());
            set.properties.insert("memory_node_id".into(), node.to_string());
            TelemetryRecord::Operational(
                OperationalObservation::from_counters(culprit_core::source_id::MEMORY_CLERKS, set).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    rows.push(unrelated);
    let CollectionTarget::Shared(member) = &mut source.target else { panic!() };
    member.native = NativeIdentity::new("private-member-canary");
    let member = member.clone();
    let SourcePartition::InstanceLocal { member: original, .. } = &mut source.partition else { panic!() };
    *original = Some(member);
    let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
    header.records = rows.len() as u32;
    header.sources[0] = if state == SourceState::Unavailable {
        StateSourceCoverage::unavailable(
            culprit_core::source_id::MEMORY_CLERKS.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else {
        StateSourceCoverage::observed(
            culprit_core::source_id::MEMORY_CLERKS.into(),
            groups as u32,
            state != SourceState::Partial,
        )
    };
    (source, rows)
}

#[tokio::test]
async fn one_local_tool_explains_real_shared_clerk_captures_with_exact_values_coverage_and_bounded_private_output() {
    use culprit_engine::LocalCaptureAnalysisKind;
    for (groups, state) in [(2, SourceState::Collected), (7, SourceState::Partial), (0, SourceState::Unavailable)] {
        let (source, rows) = clerk_originals(groups, state);
        let native = NativeReadContext::from_snapshot(&source).unwrap();
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) = culprit_core::i18n::scope_async(
            "fr",
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::MemoryClerks),
        )
        .await;
        assert!(!error, "{value}");
        assert!(value.get("page_server_reads").is_none());
        let view = &value["memory_clerks"];
        assert_eq!(
            view["title"],
            culprit_core::i18n::scope("en", || culprit_core::t!("collected.memory_clerks.title"))
        );
        assert_eq!(view["native_rows"], groups);
        assert_eq!(view["observed_rows"], groups);
        assert_eq!(view["native_source_state"], json!(state));
        assert_eq!(view["source_state"], json!(state));
        assert_eq!(view["read_complete"], true, "native availability is independent of fully verified local bytes");
        assert_eq!(view["historical_context"], true);
        assert_eq!(view["independent_shared_scope"], true);
        assert_eq!(view["original_read_bounds"], json!({"from":native.observed_from,"until":native.observed_until}));
        assert_eq!(view["entries_presented"], groups.min(5));
        assert_eq!(view["entries_limited"], groups > 5);
        assert_eq!(value["collected_from"], fixture.manifest.started_at);
        assert_eq!(value["collected_until"], fixture.manifest.finished_at.as_deref().unwrap());
        assert_eq!(value["local_verification_bytes"], fixture.original_bytes());
        if groups > 0 {
            let fields = view["entries"][0]["fields"].as_array().unwrap();
            let field = |name: &str| fields.iter().find(|field| field["key"] == name).unwrap();
            assert_eq!(field("pages_kb")["value"], "9007199254740993");
            assert_eq!(field("awe_allocated_kb")["value"], "0");
            assert_eq!(field("shared_memory_reserved_kb")["value"], serde_json::Value::Null);
            assert_eq!(view["entries"][0]["scope"], json!(culprit_core::MeasurementScope::Server));
        } else {
            assert!(view["entries"].as_array().unwrap().is_empty());
            assert!(view["source_reason"].is_string());
        }
        let text = value.to_string();
        for private in [
            "private-member-canary",
            "manifest_sha256",
            "source_sha256",
            "payload_sha256",
            "qualifiers",
            "native-process-a",
            "independent-native-counters",
        ] {
            assert!(!text.contains(private), "{private}");
        }
        assert!(text.len() <= 64 * 1024);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let (error, repeated) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::MemoryClerks).await;
        assert!(!error);
        assert_eq!(repeated, value);
    }
}

#[tokio::test]
async fn selecting_the_wrong_local_family_never_returns_an_empty_success_or_constructs_a_provider() {
    let (source, rows) = clerk_originals(2, SourceState::Collected);
    let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, calls.clone());
    let (error, value) = call(&server, &fixture.manifest.capture.observation_id).await;
    assert!(error);
    assert!(value.get("page_server_reads").is_none());
    assert!(value.get("memory_clerks").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

mod session_census;

mod instance_resources;

mod group_replication;

mod galera;

mod pool_settings;

mod managed_instance_resources;

#[tokio::test]
async fn temporal_table_configuration_uses_original_local_action_and_canonical_fields_without_a_provider() {
    use culprit_collector::analytical::test_support::{TableTemporalCase, table_temporal_evidence};
    for case in TableTemporalCase::ALL {
        let (source, records) = table_temporal_evidence(case);
        let mut fixture = LocalCaptureFixture::snapshot(source.clone(), records.clone()).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::TableTemporal).await;
        assert!(!error, "{case:?}: {value}");
        assert_eq!(value["historical_context"], true);
        let report = &value["table_temporal"];
        assert_eq!(report["native_rows"], records.len());
        assert_eq!(report["observed_rows"], records.len());
        assert_eq!(report["qualification"], culprit_core::t!("view.table_temporal.qualification"));
        if let Some(entry) = report["entries"].as_array().unwrap().first() {
            assert_eq!(entry["fields"].as_array().unwrap().len(), 6);
        }
        for private in [
            "manifest_sha256",
            "source_sha256",
            "observation_key",
            "native-machine",
            "native-member",
            "original.invalid",
        ] {
            assert!(!value.to_string().contains(private), "{private}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

mod query_wait_fragments;

mod runtime_fragments;

mod indexed_views;
