use super::*;
use crate::{native::error_log::context::tests as rows, read::test_support::Read};

fn request() -> ResourceDiscoveryRequest {
    ResourceDiscoveryRequest { kind: SharedResourceKind::Instance, budget: Default::default() }
}

fn frame() -> Vec<Result<Vec<crate::rows::Rec>>> {
    let mut replies = rows::rows();
    replies.extend(rows::rows());
    replies
}

#[tokio::test]
async fn resource_discovery_closes_and_advertises_supported_native_profiles_on_one_exact_owner() {
    let mut reader = Read::with(frame());
    let discovered = discover_from(&mut reader, &request()).await.unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].identity, rows::resource());
    assert_eq!(
        discovered[0].collections,
        [
            TelemetryCollection::Snapshot(SnapshotDataset::NativeFailures),
            TelemetryCollection::Snapshot(SnapshotDataset::SessionMemory),
            TelemetryCollection::Snapshot(SnapshotDataset::GroupReplication),
        ]
    );
    assert_eq!(reader.queries.len(), 6);
    assert_eq!(reader.closed, 1);
    assert!(reader.queries.iter().all(|(sql, _, _)| !sql.contains("log_error_services") && !sql.contains("SCHEMATA")));
}

#[tokio::test]
async fn each_native_profile_keeps_its_own_product_and_version_boundary() {
    for (version, expected) in [
        (
            "8.0.30",
            vec![
                TelemetryCollection::Snapshot(SnapshotDataset::NativeFailures),
                TelemetryCollection::Snapshot(SnapshotDataset::GroupReplication),
            ],
        ),
        ("8.0.21", vec![TelemetryCollection::Snapshot(SnapshotDataset::GroupReplication)]),
        ("5.7.17", vec![TelemetryCollection::Snapshot(SnapshotDataset::GroupReplication)]),
    ] {
        let mut replies = frame();
        for index in [0, 3] {
            replies[index] = Ok(vec![rows::platform(version, true)]);
        }
        let mut reader = Read::with(replies);
        let discovered = discover_from(&mut reader, &request()).await.unwrap();
        assert_eq!(discovered[0].collections, expected);
        assert_eq!(reader.queries.len(), 6);
        assert_eq!(reader.closed, 1);
    }
    for (version, instrumentation) in [("10.11.8-MariaDB", true), ("5.7.16", true), ("8.4.0", false)] {
        let mut reader = Read::with(vec![Ok(vec![rows::platform(version, instrumentation)])]);
        assert_eq!(discover_from(&mut reader, &request()).await.err().unwrap().code, ErrorCode::Unsupported);
        assert_eq!(reader.queries.len(), 1);
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn changed_native_identity_or_unconfirmed_shutdown_cannot_advertise_ownership() {
    let mut replies = frame();
    replies[4] = Ok(vec![rows::configuration("f54a461c-0001-4000-8000-000000000002", "")]);
    let mut reader = Read::with(replies);
    assert_eq!(
        discover_from(&mut reader, &request()).await.err().unwrap().detail,
        CollectionStop::SourceChanged.error().detail
    );
    assert_eq!(reader.closed, 1);
    let mut reader = Read::with(frame());
    reader.shutdown = Some(SourceShutdown::Unconfirmed);
    assert_eq!(
        discover_from(&mut reader, &request()).await.err().unwrap().detail,
        CollectionStop::CancellationUnconfirmed.error().detail
    );
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn discovery_cancellation_stops_at_each_native_boundary_without_source_fallback() {
    for boundary in 0..6 {
        let mut replies = frame();
        replies[boundary] = Err(CollectionStop::CancellationUnconfirmed.error());
        let mut reader = Read::with(replies);
        assert!(discover_from(&mut reader, &request()).await.is_err());
        assert_eq!(reader.queries.len(), boundary + 1);
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn mariadb_galera_discovery_uses_its_actual_native_uid_without_performance_schema_or_mysql_fallback() {
    let mut replies = crate::native::galera::tests::context_rows("2026-11-13T10:00:00.123456Z");
    replies.extend(crate::native::galera::tests::context_rows("2026-11-13T10:00:00.123458Z"));
    let mut reader = Read::with(replies);
    let discovered = discover_from(&mut reader, &request()).await.unwrap();
    assert_eq!(discovered[0].identity, crate::native::galera::tests::resource());
    assert_eq!(discovered[0].collections, [TelemetryCollection::Snapshot(SnapshotDataset::Galera)]);
    assert_eq!(reader.queries.len(), 6);
    assert_eq!(reader.closed, 1);
    assert_eq!(reader.queries[1].0, "SELECT @@GLOBAL.server_uid AS server_uid");
    assert!(
        reader.queries.iter().all(|(sql, _, _)| !sql.contains("server_uuid") && !sql.contains("performance_schema."))
    );
}
