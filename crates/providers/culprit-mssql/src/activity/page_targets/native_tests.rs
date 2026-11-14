//! Native qualification only on owned loopback databases. Workloads exist solely inside nonce fixture databases.
mod contention;
mod fixture;
mod focused;
mod workload;
use super::*;
use crate::activity::{IdentityDetail, ReadOptions};

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn local_limited_header_projection_preserves_native_file_epoch_and_bounded_empty_capture() {
    fixture::run(|fixture| {
        Box::pin(async move {
            let port = fixture.port;
            let pool = fixture.pool.as_ref().unwrap().clone();
            let options = || ReadOptions {
                budget: CollectionBudget::default(),
                scope: ActivityScope::CurrentDatabase,
                identity: IdentityDetail::Endpoint,
            };
            let mut reader = ActivityReader::open(pool.clone(), options(), "127.0.0.1".into(), port).unwrap();
            let initial = context::initial(&mut reader).await.unwrap();
            assert!(
                initial.unavailable().is_none(),
                "native mapping authority unavailable: {:?}",
                initial.unavailable()
            );
            let address = PageAddress { database: initial.authority.database_id, file: 1, page: 0 };
            let selected = BTreeSet::from([address]);
            let before = reader.page_headers(&selected).await.unwrap();
            let after = reader.page_headers(&selected).await.unwrap();
            let first = &before[&address];
            let last = &after[&address];
            assert!(address.matches(&first.header).unwrap());
            assert!(first.same_mapping(last));
            let target = mapped_target(&initial, last).unwrap();
            assert_eq!(target.identity_basis, PageTargetIdentityBasis::LocalDatabaseFileEpoch);
            assert!(target.is_valid());
            assert_eq!(target.header.page_id, 0);
            assert!(!target.file.id.is_empty());
            assert!(target.database.qualifiers.contains_key("instance_epoch"));
            assert!(reader.verify_page_wait_context(&PageWaitGuard(initial)).await.unwrap());
            assert_eq!(reader.query_count(), 8, "two bounded page reads and three native context statements per bound");
            assert_eq!(reader.close().await, SourceShutdown::Quiescent);
            let mut quiet = ActivityReader::open(pool.clone(), options(), "127.0.0.1".into(), port).unwrap();
            let capture = quiet.capture_page_wait_targets().await.unwrap();
            assert!(capture.snapshot.observations.is_empty());
            assert!(capture.snapshot.is_valid());
            assert_eq!(quiet.query_count(), 7, "empty request population must not read headers or SQL text");
            assert_eq!(quiet.close().await, SourceShutdown::Quiescent);
            drop(quiet);
            drop(reader);
            drop(pool);
            Ok(())
        })
    })
    .await;
}
