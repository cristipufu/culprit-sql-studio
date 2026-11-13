//! Opt-in native validation accepts only the owned loopback fixture's explicit port and credentials.
use super::*;
use crate::{conn::Pool, read::ReadSession, test_support::config};
use culprit_core::MeasurementScope;
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_scheduler_columns_preserve_states_nullable_flags_and_exact_shared_counters() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let pool = Arc::new(Pool::new(config(port, "master", &password, "culprit"), None));
    let mut reader = ReadSession::new(pool, CollectionBudget::default());
    let result = read(&mut reader, MAX_SCHEDULERS).await;
    let samples = reader.finish(result).await.unwrap();
    assert!(!samples.is_empty());
    assert_eq!(reader.queries(), 2);
    let mut keys = BTreeSet::new();
    for sample in samples {
        assert!(sample.is_valid());
        assert_eq!(sample.scope, MeasurementScope::Server);
        assert!(keys.insert(sample.key().unwrap()));
        assert!(sample.properties.contains_key("status"));
        assert!(sample.counters.iter().any(|counter| counter.native_name == "runnable_tasks_count"));
        assert!(sample.counters.iter().any(|counter| counter.native_name == "current_workers_count"));
        assert!(sample.counters.iter().all(|counter| counter.scope == MeasurementScope::Server));
        match sample.properties.get("failed_to_create_worker").map(String::as_str) {
            Some("0" | "1") => {}
            None => assert!(sample.properties.get("native_null_columns").is_some_and(|columns| {
                serde_json::from_str::<Vec<String>>(columns)
                    .unwrap()
                    .iter()
                    .any(|name| name == "failed_to_create_worker")
            })),
            Some(_) => panic!("native worker-creation flag must be boolean or unknown"),
        }
    }
}
