//! Genuine isolated single-node Galera acceptance; bootstrap belongs only to the nonce-owned supervisor.
use super::*;
use crate::{
    connect::Scope,
    native::{self, galera},
    read::ReadSession,
};

async fn owned() -> (crate::MysqlProvider, galera::Context) {
    let owner = std::env::var("CULPRIT_SMOKE_OWNER").expect("owned supervisor nonce");
    assert!(super::replication::valid_owner(&owner));
    assert_eq!(std::env::var("CULPRIT_MARIADB_GALERA_SINGLE_NODE").as_deref(), Ok("1"));
    let provider = super::fixture();
    // Existing setup7 + parameterized original DB/native-host admission2 + native Galera context3.
    let mut reader = ReadSession::new(
        provider.connector.clone(),
        CollectionBudget { max_queries: 12, statement_timeout_ms: 1000, total_timeout_ms: 5000, ..Default::default() },
        Scope::Server,
    );
    let acquired = Box::pin(async {
        let native = native::metadata::connection_context(&mut reader, &provider.connector).await?;
        let context = galera::context(&mut reader).await?;
        Result::Ok((native, context))
    })
    .await;
    let queries = reader.query_count();
    let shutdown = reader.close().await;
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert_eq!(queries, 12);
    let (native, context) = super::replication::required(acquired, "original nonce-owned MariaDB/Galera context");
    assert!(native.server_name == format!("culprit-smoke-mysql-{owner}"), "exact owned native hostname required");
    assert_eq!(native.database_name, "culprit_smoke");
    assert_eq!(native.connection_id, context.instance.connection_id);
    assert_eq!(
        context.metadata.enabled,
        Some(true),
        "genuine enabled Galera is required; unsupported/off is not a pass"
    );
    let expected = std::env::var("CULPRIT_MARIADB_GALERA_SERVER_UID").expect("owned native server UID witness");
    assert!(
        context.instance.resource.native.id == expected,
        "native UID must match the independent owned supervisor point"
    );
    (provider, context)
}
fn capture(
    provider: crate::MysqlProvider,
    context: &galera::Context,
) -> impl std::future::Future<Output = Box<super::capture::Capture>> {
    let request = CollectionRequest {
        subject: CollectionSubject::Shared { resource: context.instance.resource.clone() },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Galera },
        budget: crate::galera::budget(),
    };
    super::capture::original(provider, request, "owned-galera-node")
}
async fn verify_native_component() {
    let (provider, context) = Box::pin(owned()).await;
    let captured = Box::pin(capture(provider, &context)).await;
    assert_eq!(captured.manifest.status, CollectionStatus::Complete);
    assert_eq!(captured.manifest.source_queries, 17);
    let source = captured.manifest.description.as_ref().unwrap();
    assert_eq!(source.target, CollectionTarget::Shared(context.instance.resource.clone()));
    assert!(galera_context(source).unwrap().has_qualified_local_owner());
    assert_eq!(galera_context(source).unwrap().database_writable, None);
    let snapshot = source.observation.snapshot().unwrap();
    assert_eq!(snapshot.records, 1);
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Collected);
    let [record] = captured.records.as_slice() else { panic!("one actual native Galera point") };
    let TelemetryRecord::Operational(row) = &record.record else { panic!() };
    let OperationalFact::MariadbGalera(point) = &row.fact else { panic!() };
    assert!(point.fits_source(source));
    assert_eq!(point.ready, Some(true));
    assert_eq!(point.connected, Some(true));
    assert_eq!(point.component_status.as_deref(), Some("Primary"));
    assert_eq!(point.cluster_size.as_deref(), Some("1"));
    assert_eq!(point.local_state.as_deref(), Some("4"));
    assert_eq!(point.local_state_comment.as_deref(), Some("Synced"));
    assert!(point.node_uuid.as_ref().is_some_and(|value| value != "00000000-0000-0000-0000-000000000000"));
    assert!(
        point.receive_queue.is_some()
            && point.send_queue.is_some()
            && point.certification_failures.is_some()
            && point.applier_aborts.is_some()
    );
    assert!(
        ObservationBounds::new(&snapshot.observed_from, &snapshot.observed_until)
            .unwrap()
            .contains(&point.observed_from)
    );
    // No second collection/current context is used; all inspected facts came from this sealed original.
}

#[tokio::test]
#[ignore = "requires a genuine nonce-owned MariaDB single-node Galera component; supervisor owns one bootstrap and cleanup"]
async fn owned_galera_factory_outbox_retains_real_node_queues_totals_and_original_scope_without_cluster_health_claim() {
    Box::pin(verify_native_component()).await;
}
#[tokio::test]
async fn actual_native_work_futures_are_small_at_owned_query_capture_and_receipt_boundaries() {
    const MAX_FUTURE_BYTES: usize = 4096;
    // Construction only: fake native metadata supplies a Context value, never native acceptance evidence.
    let mut reader = crate::read::test_support::Read::with(galera::tests::context_rows("2026-11-13T10:00:00.123456Z"));
    let context = galera::context(&mut reader).await.unwrap();
    let provider = crate::MysqlProvider::new(&ConnectionSpec {
        engine: Some(Engine::Mysql),
        host: "127.0.0.1".into(),
        port: Some(1),
        user: Some("unused".into()),
        password: Some("unused".into()),
        ..Default::default()
    })
    .unwrap();
    let owned = owned();
    assert!(std::mem::size_of_val(&owned) <= MAX_FUTURE_BYTES);
    drop(owned);
    let capture = capture(provider, &context);
    assert!(std::mem::size_of_val(&capture) <= MAX_FUTURE_BYTES);
    drop(capture);
    let work = verify_native_component();
    assert!(std::mem::size_of_val(&work) <= MAX_FUTURE_BYTES);
    drop(work);
}
