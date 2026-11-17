//! Startup composition reaches canonical saved originals without constructing a native provider or a warehouse.
use super::*;
use clap::Parser;
use culprit_collector::{
    analytical::test_support::{LocalCaptureFixture, page_server_read_evidence_cases},
    outbox::CompletedCaptureBudget,
};
use culprit_core::{AuthKind, Engine, telemetry::*};
use culprit_engine::{NewConnection, OutboxCaptureReader, ProviderRegistration};
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
    let result = server
        .handle(
            "local-capture-test",
            "captured_page_server_reads",
            json!({
                "connection":"local-evidence", "observation_id":id,
            })
            .as_object()
            .unwrap()
            .clone(),
        )
        .await;
    (result.is_error == Some(true), serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap())
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
            assert_eq!(view["entries"][0]["statement_binding"], "matched");
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
