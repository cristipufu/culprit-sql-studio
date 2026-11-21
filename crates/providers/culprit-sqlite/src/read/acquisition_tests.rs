use super::*;
use culprit_core::{
    ProviderReadCoverage, ProviderReadDisposition, ProviderReadKind, ProviderReadObservation, ProviderSetupAccounting,
};
#[tokio::test]
async fn original_file_owner_reports_setup_but_keeps_direct_statements_explicitly_partial() {
    struct OwnedFile(std::path::PathBuf);
    impl Drop for OwnedFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let owned = OwnedFile(super::tests::fixture());
    let path = owned.0.clone();
    let owner =
        ProviderReadObservation::begin("0123456789abcdef0123456789abcdef".into(), ProviderReadKind::Probe).unwrap();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    let reader = FileReader::new(path).with_read_context(context);
    let value = reader
        .execute(Duration::from_secs(1), |connection, _| {
            connection
                .query_row("SELECT 'private-direct-canary'", [], |row| row.get::<_, String>(0))
                .map_err(|error| map_error(&error))
        })
        .await
        .unwrap();
    assert_eq!(value, "private-direct-canary");
    assert_eq!(reader.shutdown(), SourceShutdown::Quiescent);
    let _ = operation.source_finished(&Ok::<(), culprit_core::Error>(()), ProviderReadDisposition::Quiescent, Ok(()));
    let attempt = owner.finish(Ok(()));
    let summary = attempt.acquisition();
    assert!(summary.is_valid());
    assert!(matches!(summary.coverage, ProviderReadCoverage::Partial { .. }));
    assert_eq!(summary.readers[0].queries, Some(2));
    assert_eq!(summary.readers[0].setup_accounting, ProviderSetupAccounting::IncludedInReaderWork);
    assert!(!serde_json::to_string(summary).unwrap().contains("private-direct-canary"));
}
