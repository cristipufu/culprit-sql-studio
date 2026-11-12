use super::*;
use crate::{
    catalog::SchemaCatalog,
    ingestion::IngestionBatch,
    test_support::{Blobs, Queries, Recovery, Reply, returned},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use culprit_collector::{
    analytical::encoding::BatchEncoder,
    delivery::{
        recovery::reopen,
        sink::{DeliveryAttempt, TelemetrySink, VerificationBudget, dispatch_range, verify},
    },
};
use serde_json::{Value, json};
use std::{
    io::Write,
    net::{Ipv4Addr, TcpListener},
    sync::Mutex as StdMutex,
};
use zeroize::Zeroizing;

struct Tokens(StdMutex<Vec<String>>);
#[async_trait]
impl TokenProvider for Tokens {
    async fn bearer_token(&self, resource: &str) -> Result<Zeroizing<String>> {
        self.0.lock().unwrap().push(resource.into());
        Ok(Zeroizing::new("synthetic-bearer-canary".into()))
    }
}
fn query() -> AdxConfig {
    AdxConfig { endpoint: "https://cluster.westeurope.kusto.windows.net".into(), database: "db".into() }
}
fn table(columns: &[&str], rows: Value) -> Value {
    json!({"Tables":[{"Columns":columns.iter().map(|name|json!({"ColumnName":name,"DataType":"String"})).collect::<Vec<_>>(),"Rows":rows}]})
}
fn locations() -> Value {
    table(
        &["ResourceTypeName", "StorageRoot"],
        json!([
            [
                "TempStorage",
                "https://fixturestore.blob.core.windows.net/tempstorage?se=2099-01-01T00%3A00%3A00Z&sig=synthetic-sas-canary"
            ],
            [
                "SecuredReadyForAggregationQueue",
                "https://fixturestore.queue.core.windows.net/readyforaggregation-secured?se=2099-01-01T00:00:00Z&sig=synthetic-sas-canary"
            ]
        ]),
    )
}
fn identity() -> Value {
    table(&["AuthorizationContext"], json!([["synthetic-identity-canary"]]))
}
fn resources() -> Resources {
    Resources::parse(&serde_json::to_vec(&locations()).unwrap(), &serde_json::to_vec(&identity()).unwrap()).unwrap()
}
fn dispatcher(origin: &str) -> QueuedDispatcher {
    let mut resources = resources();
    // Loopback replaces only already-validated synthetic locations; production parsing never permits it.
    for resource in resources.containers.iter_mut().chain(&mut resources.queues) {
        resource.origin = origin.into();
    }
    QueuedDispatcher {
        database: "db".into(),
        service: http::Service::fixture(origin.into(), Arc::new(Tokens(StdMutex::new(vec![])))),
        resources: Mutex::new(Some(Arc::new(resources))),
        next_resource: AtomicUsize::new(0),
        deliveries: Arc::new(Semaphore::new(MAX_PARALLEL_DELIVERIES)),
    }
}
struct Server {
    origin: String,
    requests: Arc<StdMutex<Vec<Vec<u8>>>>,
    join: std::thread::JoinHandle<()>,
}
fn server(replies: Vec<(u16, String)>) -> Server {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let requests = Arc::new(StdMutex::new(vec![]));
    let seen = requests.clone();
    let join = std::thread::spawn(move || {
        for (status, body) in replies {
            let (mut socket, _) = listener.accept().unwrap();
            seen.lock().unwrap().push(crate::http_fixture::request(&mut socket).unwrap());
            write!(
                socket,
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    Server { origin, requests, join }
}
fn body(request: &[u8]) -> &[u8] {
    let start = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    &request[start..]
}
fn queued_message(request: &[u8]) -> Value {
    let text = std::str::from_utf8(body(request)).unwrap();
    let text = text
        .strip_prefix("<QueueMessage><MessageText>")
        .unwrap()
        .strip_suffix("</MessageText></QueueMessage>")
        .unwrap();
    serde_json::from_slice(&STANDARD.decode(text).unwrap()).unwrap()
}

#[test]
fn queued_delivery_uses_the_ingestion_endpoint_and_validates_before_acquiring_credentials() {
    assert_eq!(ingestion_endpoint(&query(), None).unwrap(), "https://ingest-cluster.westeurope.kusto.windows.net");
    assert_eq!(
        ingestion_endpoint(&query(), Some("https://ingest-alias.kusto.windows.net/")).unwrap(),
        "https://ingest-alias.kusto.windows.net"
    );
    for endpoint in [
        "http://cluster.kusto.windows.net",
        "https://x.kusto.windows.net@attacker.example",
        "https://cluster.kusto.windows.net/path",
        "https://cluster.kusto.windows.net?secret=canary",
    ] {
        assert!(ingestion_endpoint(&query(), Some(endpoint)).is_err());
    }
    let mut invalid = query();
    invalid.database = String::new();
    assert!(ingestion_endpoint(&invalid, None).is_err());
}

#[test]
fn metadata_requires_complete_typed_resources_and_cannot_send_sas_credentials_to_untrusted_hosts() {
    let valid = identity().to_string();
    for location in [
        "http://fixturestore.blob.core.windows.net/tempstorage?se=2099-01-01T00:00:00Z&sig=secret",
        "https://fixturestore.blob.core.windows.net.attacker.example/tempstorage?se=2099-01-01T00:00:00Z&sig=secret",
        "https://fixturestore.blob.core.windows.net@attacker.example/tempstorage?se=2099-01-01T00:00:00Z&sig=secret",
        "https://fixturestore.blob.core.windows.net/tempstorage/../outside?se=2099-01-01T00:00:00Z&sig=secret",
        "https://fixturestore.blob.core.windows.net/tempstorage?se=2000-01-01T00:00:00Z&sig=secret",
        "https://fixturestore.blob.core.windows.net/tempstorage?sig=secret",
        "https://fixturestore.blob.core.windows.net/tempstorage?se=2099-01-01T00:00:00Z",
        "https://fixturestore.blob.core.windows.net/tempstorage?se=2099-01-01T00:00:00Z&se=2099-01-01T00:00:00Z&sig=secret",
        "https://fixturestore.blob.core.windows.net:443/tempstorage?se=2099-01-01T00:00:00Z&sig=secret",
    ] {
        let mut metadata = locations();
        metadata["Tables"][0]["Rows"][0][1] = json!(location);
        assert!(Resources::parse(metadata.to_string().as_bytes(), valid.as_bytes()).is_err(), "{location}");
    }
    for metadata in
        [json!({}), json!({"error":{"message":"secret"}}), table(&["ResourceTypeName", "StorageRoot"], json!([]))]
    {
        assert!(Resources::parse(metadata.to_string().as_bytes(), valid.as_bytes()).is_err());
    }
    let metadata = locations().to_string();
    for identity in [
        json!({}),
        table(&["AuthorizationContext"], json!([])),
        table(&["AuthorizationContext"], json!([[""]])),
        table(&["AuthorizationContext"], json!([["one"], ["two"]])),
    ] {
        assert!(Resources::parse(metadata.as_bytes(), identity.to_string().as_bytes()).is_err());
    }
    let valid_resources = resources();
    assert!(valid_resources.usable());
    valid_resources.invalidate();
    assert!(!valid_resources.usable());
    let mut near_expiry = locations();
    near_expiry["Tables"][0]["Rows"][0][1] = json!(format!(
        "https://fixturestore.blob.core.windows.net/tempstorage?se={}&sig=synthetic",
        (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));
    assert!(Resources::parse(near_expiry.to_string().as_bytes(), valid.as_bytes()).is_err());
    let mut duplicate_tables = locations();
    duplicate_tables["Tables"].as_array_mut().unwrap().push(locations()["Tables"][0].clone());
    assert!(Resources::parse(duplicate_tables.to_string().as_bytes(), valid.as_bytes()).is_err());
    assert!(Resources::parse(&vec![b' '; 65537], valid.as_bytes()).is_err());
}

#[tokio::test]
async fn rejected_resource_discovery_stops_before_upload_without_exposing_service_secrets() {
    let server = server(vec![(403, "synthetic-sas-canary".into())]);
    let queued = dispatcher(&server.origin);
    *queued.resources.lock().await = None;
    let error = queued.resources().await.err().unwrap();
    assert!(!format!("{error:?}").contains("synthetic-sas-canary"));
    assert!(queued.resources.lock().await.is_none());
    server.join.join().unwrap();
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn service_resources_are_cached_and_refreshed_after_expiry_or_an_ambiguous_delivery() {
    let replies = (0..3).flat_map(|_| [(200, locations().to_string()), (200, identity().to_string())]).collect();
    let server = server(replies);
    let tokens = Arc::new(Tokens(StdMutex::new(vec![])));
    let mut dispatcher = dispatcher(&server.origin);
    dispatcher.service = http::Service::fixture(server.origin.clone(), tokens.clone());
    *dispatcher.resources.lock().await = None;
    let first = dispatcher.resources().await.unwrap();
    assert!(Arc::ptr_eq(&first, &dispatcher.resources().await.unwrap()));
    first.invalidate();
    let second = dispatcher.resources().await.unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    tokio::time::advance(std::time::Duration::from_secs(6 * 60)).await;
    dispatcher.resources().await.unwrap();
    server.join.join().unwrap();
    assert_eq!(tokens.0.lock().unwrap().as_slice(), ["https://kusto.kusto.windows.net"; 3]);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    for request in requests.iter() {
        let text = String::from_utf8_lossy(request).to_ascii_lowercase();
        assert!(text.starts_with("post /v1/rest/mgmt "));
        assert!(text.contains("authorization: bearer synthetic-bearer-canary"));
        assert!(!text.contains("x-ms-readonly"));
    }
}

#[tokio::test]
async fn queued_batches_and_publication_share_exact_readback_and_never_trust_queue_acceptance() {
    let server = server(vec![(201, String::new()); 4]);
    let queued = Arc::new(dispatcher(&server.origin));
    let scratch = tempfile::tempdir().unwrap();
    let catalog = SchemaCatalog::new().unwrap();
    let recovery = Arc::new(Recovery::new("adx:test", catalog.digest()));
    let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
    let queries = Arc::new(Queries::default());
    let sink = crate::sink::new(
        range.manifest().contract.clone(),
        recovery,
        Arc::new(Blobs::default()),
        queued,
        queries.clone(),
        scratch.path(),
    )
    .unwrap();
    let attempt = DeliveryAttempt::new(1).unwrap();
    dispatch_range(&sink, &range, &attempt).await.unwrap();
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    queries.replies.lock().unwrap().extend([Reply::Audit(0), Reply::Rows(vec![])]);
    assert!(verify(&sink, &range, VerificationBudget::default()).await.is_err());
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    let row: Value = serde_json::from_slice(body(&server.requests.lock().unwrap()[0])).unwrap();
    let schema = catalog.get(range.manifest().batches[0].stream).unwrap();
    queries.replies.lock().unwrap().extend([Reply::Audit(0), Reply::Rows(vec![returned(schema, &row)])]);
    let verified = verify(&sink, &range, VerificationBudget::default()).await.unwrap();
    let publication = sink.publish(&verified, &attempt).await.unwrap();
    queries.replies.lock().unwrap().push_back(Reply::Rows(vec![]));
    assert!(!sink.publication_visible(&publication, &range).await.unwrap());
    let control: Value = serde_json::from_slice(body(&server.requests.lock().unwrap()[2])).unwrap();
    queries.replies.lock().unwrap().push_back(Reply::Rows(vec![returned(catalog.publication(), &control)]));
    assert!(sink.publication_visible(&publication, &range).await.unwrap());
    server.join.join().unwrap();
    let requests = server.requests.lock().unwrap();
    for (upload, submission) in [(0, 1), (2, 3)] {
        let headers = String::from_utf8_lossy(&requests[upload]).to_ascii_lowercase();
        assert!(headers.starts_with("put /tempstorage/ready/culprit"));
        assert!(!headers.contains("authorization:"));
        let headers = String::from_utf8_lossy(&requests[submission]).to_ascii_lowercase();
        assert!(headers.starts_with("post /readyforaggregation-secured/messages?"));
        assert!(!headers.contains("authorization:"));
        for request in [&requests[upload], &requests[submission]] {
            let text = String::from_utf8_lossy(request);
            let date = text.lines().find_map(|line| line.strip_prefix("x-ms-date: ")).unwrap();
            chrono::DateTime::parse_from_rfc2822(date).unwrap();
        }
        let message = queued_message(&requests[submission]);
        assert_eq!(message["RawDataSize"], body(&requests[upload]).len());
        assert_eq!(message["FlushImmediately"], false);
        assert_eq!(message["RetainBlobOnSuccess"], false);
        assert_eq!(message["AdditionalProperties"]["authorizationContext"], "synthetic-identity-canary");
        assert_eq!(message["AdditionalProperties"]["format"], "json");
        assert_eq!(
            message["AdditionalProperties"]["mappingReference"],
            format!("{}Mapping", message["TableName"].as_str().unwrap())
        );
    }
    assert_eq!(queued_message(&requests[3])["TableName"], "CulpritPublishedRangesV1");
}

#[tokio::test]
async fn a_failed_upload_never_enqueues_and_a_failed_enqueue_invalidates_its_resource_cache() {
    for replies in
        [vec![(503, "synthetic-sas-canary".into())], vec![(201, String::new()), (503, "synthetic-sas-canary".into())]]
    {
        let count = replies.len();
        let server = server(replies);
        let queued = dispatcher(&server.origin);
        let scratch = tempfile::tempdir().unwrap();
        let catalog = Arc::new(SchemaCatalog::new().unwrap());
        let recovery = Arc::new(Recovery::new("adx:test", catalog.digest()));
        let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
        let encoder = BatchEncoder::new(catalog, recovery, scratch.path()).unwrap();
        let encoded = Arc::new(encoder.encode(&range, &range.manifest().batches[0].batch_id).await.unwrap());
        let batch = Arc::new(IngestionBatch::encoded(&range, &DeliveryAttempt::new(1).unwrap(), encoded).unwrap());
        let error = queued.submit(batch).await.unwrap_err();
        assert!(!format!("{error:?}").contains("synthetic-sas-canary"));
        assert!(!queued.resources.lock().await.as_ref().unwrap().usable());
        server.join.join().unwrap();
        assert_eq!(server.requests.lock().unwrap().len(), count);
        assert_eq!(queued.deliveries.available_permits(), MAX_PARALLEL_DELIVERIES);
    }
}

#[tokio::test]
async fn cancellation_keeps_the_native_upload_owner_and_batch_until_queue_completion() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut upload, _) = listener.accept().unwrap();
        crate::http_fixture::request(&mut upload).unwrap();
        write!(upload, "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        let (mut queue, _) = listener.accept().unwrap();
        crate::http_fixture::request(&mut queue).unwrap();
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        write!(queue, "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let queued = Arc::new(dispatcher(&origin));
    let scratch = tempfile::tempdir().unwrap();
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery = Arc::new(Recovery::new("adx:test", catalog.digest()));
    let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
    let encoder = BatchEncoder::new(catalog, recovery, scratch.path()).unwrap();
    let encoded = Arc::new(encoder.encode(&range, &range.manifest().batches[0].batch_id).await.unwrap());
    let held = Arc::downgrade(&encoded);
    let batch = Arc::new(IngestionBatch::encoded(&range, &DeliveryAttempt::new(1).unwrap(), encoded).unwrap());
    let dispatcher = queued.clone();
    let task = tokio::spawn(async move { dispatcher.submit(batch).await });
    started_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(held.upgrade().is_some());
    assert_eq!(queued.deliveries.available_permits(), MAX_PARALLEL_DELIVERIES - 1);
    release_tx.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while held.upgrade().is_some() || queued.deliveries.available_permits() != MAX_PARALLEL_DELIVERIES {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    server.join().unwrap();
}
