use super::*;
use culprit_core::{
    ConnectionSpec, ProviderReadCoverage, ProviderReadDisposition, ProviderReadKind, ProviderReadObservation,
    ProviderSetupAccounting,
};
fn connector() -> Connector {
    Connector::new(&ConnectionSpec {
        engine: Some(Engine::Postgres),
        host: "no-native-dispatch.invalid".into(),
        user: Some("monitor".into()),
        ..Default::default()
    })
    .unwrap()
}
#[tokio::test]
async fn normal_wire_setup_is_counted_in_the_same_reader_without_a_second_allowance() {
    let owner =
        ProviderReadObservation::begin("0123456789abcdef0123456789abcdef".into(), ProviderReadKind::Probe).unwrap();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    context.complete_reader_coverage();
    let scope = ApplicationReadScope::new();
    let mut reader =
        ReadSession::application(connector().with_read_context(context), crate::connect::COLLECTOR, scope.clone());
    assert_eq!(reader.deadline, scope.deadline);
    assert_eq!(reader.queries, 0);
    assert!(!reader.initialized);
    assert!(reader.client.is_none());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let _ = operation.source_finished(&Ok::<(), culprit_core::Error>(()), ProviderReadDisposition::NotStarted, Ok(()));
    let attempt = owner.finish(Ok(()));
    let summary = attempt.acquisition();
    assert!(summary.is_valid());
    assert_eq!(summary.coverage, ProviderReadCoverage::Complete);
    assert_eq!(summary.readers[0].queries, Some(0));
    assert_eq!(summary.readers[0].setup_queries, None);
    assert_eq!(summary.readers[0].setup_accounting, ProviderSetupAccounting::IncludedInReaderWork);
}
#[test]
fn native_telemetry_constructor_keeps_its_original_budget_and_no_normal_observer() {
    let budget = CollectionBudget { max_queries: 7, ..Default::default() };
    let reader = ReadSession::new(connector(), budget.clone());
    assert_eq!(reader.budget, budget);
    assert!(reader.observation.is_none());
}
