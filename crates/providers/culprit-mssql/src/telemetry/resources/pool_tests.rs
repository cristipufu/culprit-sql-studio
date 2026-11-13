use super::*;
use crate::resources::{
    pool::test_support::*,
    test_support::{FakeRead, inventory_row, replace},
};
use crate::value::{Rec, Val};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Shared {
            resource: SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Pool,
                namespace: NativeIdentity::new("fixture-server").qualified("kind", "azure-sql-logical-server"),
                native: NativeIdentity::new("fixture-pool"),
            },
        },
        selection: CollectionSelection::ResourceHistory {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T06:01:00Z".into(),
        },
        budget: CollectionBudget::default(),
    }
}
fn edition() -> crate::read::ReadResult<Vec<Rec>> {
    Ok(vec![Rec::of(&[("edition", Val::Int(5))])])
}

#[tokio::test]
async fn the_provider_admits_only_its_supported_shared_route_without_opening_a_connection() {
    use culprit_core::Provider;
    let provider = crate::MssqlProvider::new(&culprit_core::ConnectionSpec {
        engine: Some(culprit_core::Engine::Mssql),
        host: "never-connect.fixture.invalid".into(),
        database: Some("fixture".into()),
        user: Some("reader".into()),
        ..Default::default()
    })
    .unwrap();
    let mut wanted = request();
    let mut source = provider.telemetry_source(&wanted).unwrap();
    assert_eq!(source.query_count(), 0);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    wanted.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational };
    assert!(
        provider.telemetry_source(&wanted).is_err(),
        "instance/host statistics do not prove elastic-pool ownership"
    );
    let CollectionSubject::Shared { resource } = &mut wanted.subject else { unreachable!() };
    resource.kind = SharedResourceKind::Instance;
    let mut shared_state = provider.telemetry_source(&wanted).unwrap();
    assert_eq!(shared_state.query_count(), 0);
    assert_eq!(shared_state.close().await, SourceShutdown::Quiescent);
    wanted.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::WaitCounters };
    assert!(provider.telemetry_source(&wanted).is_err());
    wanted = request();
    let CollectionSubject::Shared { resource } = &mut wanted.subject else { unreachable!() };
    resource.engine = culprit_core::Engine::Postgres;
    assert!(provider.telemetry_source(&wanted).is_err());
    assert_eq!(
        provider
            .telemetry_resources(&ResourceDiscoveryRequest {
                kind: SharedResourceKind::Node,
                budget: CollectionBudget::default()
            })
            .await
            .err()
            .unwrap()
            .code,
        culprit_core::ErrorCode::Unsupported
    );
}
fn source(replies: Vec<crate::read::ReadResult<Vec<Rec>>>) -> ResourceSource<FakeRead, PoolScope> {
    ResourceSource::with_scope(FakeRead::new(replies), &request(), PoolScope).unwrap()
}

#[tokio::test]
async fn explicit_pool_capture_uses_the_common_lifecycle_without_a_synthetic_database_or_replica() {
    let mut source = source(vec![
        edition(),
        Ok(vec![identity_row()]),
        Ok(vec![inventory_row()]),
        Ok(vec![identity_row()]),
        Ok(vec![row("2020-01-01T06:00:15Z"), row("2020-01-01T06:00:30Z")]),
        Ok(vec![identity_row()]),
        Ok(vec![inventory_row()]),
    ]);
    let description = source.describe().await.unwrap();
    assert!(matches!(description.target, CollectionTarget::Shared(_)));
    assert_eq!(description.source, culprit_core::source_id::POOL_RESOURCES);
    let SourceObservation::RetainedSamples(history) = &description.observation else { panic!() };
    assert_eq!(history.nominal_period_ms, None);
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::ResourceSamples, None).await.unwrap()
    else {
        panic!()
    };
    assert!(next_cursor.is_none());
    assert_eq!(records.len(), 2);
    let capture = CaptureIdentity { observation_id: "pool-capture".into(), revision: None };
    for record in records {
        let exported = ExportedRecord::new(&description, &capture, record).unwrap();
        let TelemetryRecord::ResourceSample(sample) = exported.record else { panic!() };
        assert!(sample.replica_id.is_none());
        assert!(sample.is_valid());
    }
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.query_count(), 7);
    assert_eq!(source.session.closed, 1);
}

#[tokio::test]
async fn a_wrong_pool_or_namespace_stops_before_history_is_read() {
    for (column, value) in [("pool_name", "another-pool"), ("server_name", "another-server")] {
        let mut source =
            source(vec![edition(), Ok(vec![replace(&identity_row(), &[(column, Val::Text(value.into()))])])]);
        let error = source.describe().await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert_eq!(source.query_count(), 2);
        assert!(source.read_page(TelemetryStream::ResourceSamples, None).await.is_err());
        assert_eq!(source.query_count(), 2);
        source.close().await;
    }
}

#[tokio::test]
async fn membership_moves_are_rechecked_before_each_page_and_recreated_member_storage_prevents_publication() {
    let moved = replace(&identity_row(), &[("pool_name", Val::Text("another-pool".into()))]);
    let mut source = source(vec![edition(), Ok(vec![identity_row()]), Ok(vec![inventory_row()]), Ok(vec![moved])]);
    source.describe().await.unwrap();
    assert!(source.read_page(TelemetryStream::ResourceSamples, None).await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 4);
    assert!(!source.session.calls.iter().any(|call| call.sql.contains("SELECT TOP (@P1)")));

    let changed = replace(&identity_row(), &[("physical_incarnation", Val::Text("replacement-storage".into()))]);
    let mut source = self::source(vec![
        edition(),
        Ok(vec![identity_row()]),
        Ok(vec![inventory_row()]),
        Ok(vec![identity_row()]),
        Ok(vec![row("2020-01-01T06:00:15Z"), row("2020-01-01T06:00:30Z")]),
        Ok(vec![changed]),
    ]);
    source.describe().await.unwrap();
    source.read_page(TelemetryStream::ResourceSamples, None).await.unwrap();
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.query_count(), 6);
}

#[tokio::test]
async fn discovery_is_bounded_metadata_and_does_not_turn_a_nonpooled_database_into_a_resource() {
    let request = ResourceDiscoveryRequest { kind: SharedResourceKind::Pool, budget: CollectionBudget::default() };
    let mut reader = FakeRead::new(vec![edition(), Ok(vec![identity_row()])]);
    let resources = discover_with_io(&mut reader, &request).await.unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(
        resources[0].collections,
        vec![TelemetryCollection::ResourceHistory, TelemetryCollection::Snapshot(SnapshotDataset::MemoryFailures)]
    );
    assert_eq!(reader.calls.len(), 2);
    assert!(reader.calls.iter().all(|call| call.limit <= 2));
    let mut reader = FakeRead::new(vec![edition(), Ok(vec![replace(&identity_row(), &[("pool_name", Val::Null)])])]);
    assert!(discover_with_io(&mut reader, &request).await.unwrap().is_empty());
    let mut reader = FakeRead::new(vec![
        edition(),
        Err(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denied").into()),
    ]);
    assert!(discover_with_io(&mut reader, &request).await.is_err());
}

#[tokio::test]
async fn native_head_lag_waits_for_the_exact_window_without_reading_sample_payloads() {
    let lagging = replace(&inventory_row(), &[("available_until", Val::Text("2020-01-01T06:00:30Z".into()))]);
    let mut source = source(vec![edition(), Ok(vec![identity_row()]), Ok(vec![lagging])]);
    source.describe().await.unwrap();
    assert!(matches!(
        source.read_page(TelemetryStream::ResourceSamples, None).await.unwrap(),
        CollectionRead::Stopped(CollectionStop::SourceNotReady)
    ));
    assert_eq!(source.query_count(), 3);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
