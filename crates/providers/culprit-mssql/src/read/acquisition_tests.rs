use super::*;
use culprit_core::{
    ProviderReadCoverage, ProviderReadDisposition, ProviderReadKind, ProviderReadObservation, ProviderSetupAccounting,
};
#[tokio::test]
async fn observed_read_owner_records_measured_zero_setup_without_dispatching_or_renewing_its_budget() {
    let owner =
        ProviderReadObservation::begin("0123456789abcdef0123456789abcdef".into(), ProviderReadKind::Probe).unwrap();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    context.complete_reader_coverage();
    let mut pool = Pool::new(tiberius::Config::new(), None);
    pool.observation = context;
    let budget = CollectionBudget { max_queries: 7, ..Default::default() };
    let mut reader = ReadSession::new(Arc::new(pool), budget.clone());
    assert_eq!(reader.budget, budget);
    assert_eq!(reader.queries(), 0);
    assert!(reader.client.is_none());
    assert_eq!(reader.close().await, culprit_core::telemetry::SourceShutdown::Quiescent);
    let _ = operation.source_finished(&Ok::<(), culprit_core::Error>(()), ProviderReadDisposition::NotStarted, Ok(()));
    let attempt = owner.finish(Ok(()));
    let summary = attempt.acquisition();
    assert!(summary.is_valid());
    assert_eq!(summary.coverage, ProviderReadCoverage::Complete);
    assert_eq!(summary.readers[0].queries, Some(0));
    assert_eq!(summary.readers[0].setup_queries, Some(0));
    assert_eq!(summary.readers[0].setup_accounting, ProviderSetupAccounting::Separate);
    assert_eq!(reader.budget, budget);
}
#[test]
fn source_and_qualification_readers_remain_unobserved_without_a_normal_invocation_context() {
    let pool = Arc::new(Pool::new(tiberius::Config::new(), None));
    let reader = ReadSession::new(pool, CollectionBudget::default());
    assert!(reader.observation.is_none());
    assert!(reader.connection_work.is_none());
    assert!(reader.session_setup.is_none());
}
