use super::*;
use culprit_collector::analytical::test_support::{NumaNodeCase, numa_node_evidence};
#[tokio::test]
async fn selected_native_node_original_survives_real_cli_save_offline_replay_and_share_withdrawal() {
    let (source, rows) = numa_node_evidence(NumaNodeCase::PartialMemory);
    let mut fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let workspace = workspace(&fixture, calls.clone(), None);
    let parsed = args(&[
        "--evidence-outbox",
        fixture.directory.to_str().unwrap(),
        "--numa-nodes-capture",
        &fixture.manifest.capture.observation_id,
        "--plans",
        "0",
    ])
    .unwrap();
    let report = scan(&workspace, &parsed).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let original = &report.snapshot.operations.numa_nodes[0];
    let view = &report.numa_nodes.as_ref().unwrap()[0];
    let nodes = view.numa_nodes.as_ref().unwrap();
    assert_eq!(nodes.populations.len(), 2);
    assert_eq!(nodes.populations[1].source_state, culprit_core::SourceState::Partial);
    assert_eq!(nodes.context.as_ref().unwrap().native().unwrap().partition, source.partition);
    assert!(view.groups.is_empty());
    let path = workspace.paths().reports_dir("local-evidence").join(format!("{}.json", report.snapshot.id));
    let offline = Workspace::open_in(fixture.directory.parent().unwrap().join("offline")).with_provider(
        Engine::Mssql,
        ProviderRegistration::new(Arc::new(|_| panic!("source-free replay must not construct a provider")), vec![])
            .unwrap(),
    );
    let saved = offline.analyze_file(&path).unwrap();
    assert_eq!(&saved.snapshot.operations.numa_nodes[0], original);
    assert_eq!(serde_json::to_value(saved.numa_nodes).unwrap(), serde_json::to_value(&report.numa_nodes).unwrap());
    let shared = culprit_core::redact::redact_snapshot(&report.snapshot);
    let shared = culprit_checks::analyze(shared);
    let withdrawn = shared.numa_nodes.as_ref().unwrap()[0].numa_nodes.as_ref().unwrap();
    assert!(withdrawn.context.is_none() && withdrawn.populations.is_empty());
    assert!(!withdrawn.read_complete);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let compact = crate::output::compact_report(&report, &path);
    assert_eq!(compact["numa_nodes"][0]["populations"].as_array().unwrap().len(), 2);
    let text = crate::output::observations::instance_resources(view);
    assert!(text.contains(&nodes.populations[0].title) && text.contains(&nodes.populations[1].title));
    assert!(!text.contains("source_sha256"));
}
#[test]
fn node_capture_is_explicit_and_does_not_enable_source_collection_or_accept_a_second_original() {
    assert!(args(&["--numa-nodes-capture", "original"]).is_err());
    assert!(args(&["--evidence-outbox", "outbox", "--numa-nodes-capture", "one", "--cdc-capture", "two"]).is_err());
    let args = args(&["--evidence-outbox", "outbox", "--numa-nodes-capture", "one"]).unwrap();
    assert!(!args.snapshot_options().collect_shared_resources);
}
