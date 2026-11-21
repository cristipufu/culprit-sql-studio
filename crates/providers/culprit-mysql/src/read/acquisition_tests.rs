use super::*;
use culprit_core::{
    ConnectionSpec, ProviderReadCoverage, ProviderReadDisposition, ProviderReadKind, ProviderReadObservation,
    ProviderSetupAccounting,
};
fn connector() -> Connector {
    Connector::new(&ConnectionSpec {
        engine: Some(Engine::Mysql),
        host: "no-native-dispatch.invalid".into(),
        user: Some("monitor".into()),
        ..Default::default()
    })
    .unwrap()
}
#[tokio::test]
async fn observed_client_and_control_accounting_share_one_original_reader_and_allowance() {
    let owner =
        ProviderReadObservation::begin("0123456789abcdef0123456789abcdef".into(), ProviderReadKind::Probe).unwrap();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    context.complete_reader_coverage();
    let mut connector = connector();
    connector.observation = context;
    let budget = CollectionBudget { max_queries: 3, ..Default::default() };
    let mut reader = ReadSession::new(Arc::new(connector), budget.clone(), Scope::Server);
    let clone = reader.accounting.clone();
    clone.control_query("SELECT private_control_canary").complete(&Ok::<(), mysql_async::Error>(()), 0, 0);
    assert_eq!(reader.query_count(), 1);
    assert_eq!(reader.accounting.budget, budget);
    assert!(reader.peer.is_none());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let _ = operation.source_finished(&Ok::<(), culprit_core::Error>(()), ProviderReadDisposition::NotStarted, Ok(()));
    let attempt = owner.finish(Ok(()));
    let summary = attempt.acquisition();
    assert!(summary.is_valid());
    assert_eq!(summary.coverage, ProviderReadCoverage::Complete);
    assert_eq!(summary.readers.len(), 1);
    assert_eq!(summary.readers[0].queries, Some(1));
    assert_eq!(summary.readers[0].setup_accounting, ProviderSetupAccounting::IncludedInReaderWork);
    assert!(!serde_json::to_string(summary).unwrap().contains("private_control_canary"));
}
#[test]
fn source_readers_with_no_normal_context_keep_their_original_telemetry_accounting() {
    let budget = CollectionBudget { max_queries: 3, ..Default::default() };
    let reader = ReadSession::new(Arc::new(connector()), budget.clone(), Scope::Server);
    assert_eq!(reader.accounting.budget, budget);
    assert!(reader.observation.is_none());
    assert!(reader.accounting.observation.is_none());
}
