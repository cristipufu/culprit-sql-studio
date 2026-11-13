use super::*;
use culprit_auth::AccessKeyCredential;
use tokio::io::AsyncReadExt;
use zeroize::Zeroizing;

fn server(response: Vec<u8>) -> (u16, std::thread::JoinHandle<String>) {
    use std::io::Write;
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let request = crate::http_fixture::request(&mut socket).unwrap();
        socket.write_all(&response).unwrap();
        String::from_utf8_lossy(&request).into_owned()
    });
    (port, task)
}
fn transport_at(port: u16) -> S3Transport {
    S3Transport::new(config(Some(format!("http://127.0.0.1:{port}"))), Arc::new(Keys)).unwrap()
}

pub(super) struct Keys;
#[async_trait]
impl AccessKeyProvider for Keys {
    async fn access_keys(&self) -> Result<AccessKeyCredential> {
        Ok(AccessKeyCredential {
            key_id: Zeroizing::new("fixture-access-key".into()),
            secret_key: Zeroizing::new("fixture-secret-key".into()),
            session_token: None,
            expires_at: None,
        })
    }
}
fn config(endpoint: Option<String>) -> S3Config {
    S3Config {
        bucket: "culprit-recovery".into(),
        region: "us-east-1".into(),
        prefix: "pipeline/shard".into(),
        endpoint,
        expected_owner: Some("123456789012".into()),
    }
}
#[test]
fn endpoint_policy_refuses_remote_plaintext_credentials_paths_and_ambiguous_namespaces() {
    assert!(S3Transport::new(config(None), Arc::new(Keys)).is_ok());
    for endpoint in [
        "http://s3.example.com",
        "https://user:secret@s3.example.com",
        "https://s3.example.com/path",
        "https://s3.example.com/?sig=secret",
        "https://s3.example.com/#fragment",
    ] {
        assert!(S3Transport::new(config(Some(endpoint.into())), Arc::new(Keys)).is_err(), "{endpoint}");
    }
    let endpoint = config(None).endpoint().unwrap();
    for key in ["../outside", "x/../y", "/absolute", "x?secret", "x%2fy", "x//y", "x\\y"] {
        assert!(endpoint.path(key).is_err());
    }
    assert_eq!(endpoint.origin, "https://culprit-recovery.s3.us-east-1.amazonaws.com");
    assert_eq!(endpoint.path("payloads/hash.ndjson").unwrap(), "/pipeline/shard/payloads/hash.ndjson");
}

#[tokio::test]
async fn signature_matches_the_published_aws_get_object_vector() {
    // https://docs.aws.amazon.com/AmazonS3/latest/developerguide/sig-v4-header-based-auth.html
    let endpoint = Endpoint {
        origin: "https://examplebucket.s3.amazonaws.com".into(),
        host: "examplebucket.s3.amazonaws.com".into(),
        prefix: String::new(),
        region: "us-east-1".into(),
        expected_owner: None,
    };
    let credentials = AccessKeyCredential {
        key_id: Zeroizing::new("AKIAIOSFODNN7EXAMPLE".into()),
        secret_key: Zeroizing::new("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into()),
        session_token: None,
        expires_at: None,
    };
    let date = "2013-05-24T00:00:00Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap();
    let signed = signing::sign(
        "GET",
        "/test.txt",
        &endpoint,
        &credentials,
        &signing::sha256(b""),
        Some(("range", "bytes=0-9")),
        date,
    )
    .unwrap();
    assert!(
        signed.headers["authorization"]
            .ends_with("Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41")
    );
}

#[tokio::test]
async fn native_write_results_preserve_preconditions_and_never_hide_ambiguous_outcomes() {
    for status in [200, 412, 409, 403, 500, 302] {
        let (port, server) = server(
            format!("HTTP/1.1 {status} Result\r\nContent-Length: 17\r\nConnection: close\r\n\r\nprivate-error-msg")
                .into_bytes(),
        );
        let result = transport_at(port).put("state.json", b"{}".to_vec(), WriteCondition::Matches("\"old\"")).await;
        match status {
            200 => assert!(matches!(result, Ok(ObjectWrite::Written))),
            412 => assert!(matches!(result, Ok(ObjectWrite::ConditionNotMet))),
            _ => assert!(!format!("{:?}", result.err().unwrap()).contains("private-error-msg")),
        }
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.contains("if-match: \"old\""));
        assert!(request.contains("signedheaders=host;if-match;"));
        assert!(
            request.contains("x-amz-content-sha256: 44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a")
        );
    }
}

#[tokio::test]
async fn native_reads_reject_partial_redirected_encoded_and_truncated_responses() {
    for headers in [
        "206 Partial Content\r\nContent-Length: 0",
        "302 Found\r\nContent-Length: 0",
        "200 OK\r\nContent-Length: 0\r\nContent-Encoding: gzip",
        "200 OK\r\nContent-Length: 0\r\nContent-Range: bytes 0-0/1",
    ] {
        let (port, server) =
            server(format!("HTTP/1.1 {headers}\r\nETag: \"one\"\r\nConnection: close\r\n\r\n").into_bytes());
        assert!(transport_at(port).get("payload").await.is_err());
        server.join().unwrap();
    }
    let (port, server) =
        server(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nETag: \"one\"\r\nConnection: close\r\n\r\nshort".to_vec());
    let mut object = transport_at(port).get("payload").await.unwrap().unwrap();
    assert!(object.body.read_to_end(&mut vec![]).await.is_err());
    server.join().unwrap();
}

#[tokio::test]
#[ignore = "requires the owned loopback S3 fixture; CULPRIT_TEST_S3_PORT"]
async fn s3_conditional_checkpoint_and_streamed_payload_roundtrip() {
    let port = std::env::var("CULPRIT_TEST_S3_PORT").expect("owned fixture port").parse::<u16>().unwrap();
    let settings = config(Some(format!("http://127.0.0.1:{port}")));
    let transport = owned_transport(settings).await;
    culprit_collector::delivery::object_store::test_support::verify_recovery_contract(transport).await;
}

async fn owned_transport(settings: S3Config) -> Arc<S3Transport> {
    let endpoint = settings.endpoint().unwrap();
    let keys = Keys.access_keys().await.unwrap();
    let path = format!("/{}", settings.bucket);
    let headers =
        signing::sign("PUT", &path, &endpoint, &keys, &signing::sha256(b""), None, chrono::Utc::now()).unwrap();
    tokio::task::spawn_blocking(move || {
        let mut request = agent(false).put(format!("{}{path}", endpoint.origin));
        for (key, value) in &headers.headers {
            request = request.header(key, value.as_str());
        }
        assert_eq!(request.send_empty().unwrap().status().as_u16(), 200);
    })
    .await
    .unwrap();
    Arc::new(S3Transport::new(settings, Arc::new(Keys)).unwrap())
}

#[tokio::test]
#[ignore = "requires the owned S3 and ClickHouse fixtures from dev/smoke-s3.py"]
async fn s3_recovery_clickhouse_delivery_and_restart() {
    use culprit_collector::{
        analytical::test_support,
        delivery::{
            object_store::ObjectRecoveryStore,
            recovery::{PayloadObject, RecoveryStore, reopen},
            sink::{DeliveryAttempt, TelemetrySink, VerificationBudget, dispatch_range, verify},
        },
    };
    use culprit_store_clickhouse::{
        catalog::SchemaCatalog,
        client::{ClickHouseConfig, Credentials},
        deployment::DeploymentAssets,
        ingestion::{ClickHouseIngestor, WritePolicy},
    };
    let s3_port = std::env::var("CULPRIT_TEST_S3_PORT").unwrap().parse::<u16>().unwrap();
    let ch_port = std::env::var("CULPRIT_TEST_CLICKHOUSE_PORT").unwrap().parse::<u16>().unwrap();
    let mut settings = config(Some(format!("http://127.0.0.1:{s3_port}")));
    settings.bucket = "delivery-fixture".into();
    let objects = owned_transport(settings).await;
    let endpoint = format!("http://127.0.0.1:{ch_port}");
    let origin = endpoint.clone();
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        for sql in DeploymentAssets::new().unwrap().commands {
            let response = agent
                .post(format!("{origin}/"))
                .header("X-ClickHouse-User", "culprit_fixture")
                .header("X-ClickHouse-Key", "owned-fixture-password")
                .query("database", "culprit_fixture")
                .send(format!("/* culprit */ {sql}"))
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
        }
    })
    .await
    .unwrap();
    let ingestion = Arc::new(
        ClickHouseIngestor::new(
            ClickHouseConfig { endpoint, database: "culprit_fixture".into() },
            Credentials::new("culprit_fixture".into(), Zeroizing::new("owned-fixture-password".into())).unwrap(),
            WritePolicy::default(),
        )
        .unwrap(),
    );
    let client = ingestion.query_transport();
    let (mut manifest, row) = test_support::fixture();
    manifest.contract = culprit_collector::delivery::contract::Contract::new(
        "s3-clickhouse".into(),
        "shard-0".into(),
        client.destination().into(),
        SchemaCatalog::new().unwrap().digest().into(),
    )
    .unwrap();
    manifest.range_id =
        culprit_core::telemetry::digest(&serde_json::to_vec(&(&manifest.contract, &manifest.capture)).unwrap());
    let store = Arc::new(ObjectRecoveryStore::create(objects.clone(), manifest.contract.clone()).await.unwrap());
    let owner = store.acquire().await.unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("native.ndjson");
    let mut bytes = serde_json::to_vec(&row).unwrap();
    bytes.push(b'\n');
    std::fs::write(&path, &bytes).unwrap();
    let payload = PayloadObject::from(&manifest.batches[0]);
    let staged = store.stage(&payload, &path).await.unwrap();
    store.commit(&owner, None, &manifest, &[staged]).await.unwrap();
    let range = reopen(store.as_ref(), &manifest.range_id).await.unwrap();
    let sink = culprit_store_clickhouse::sink::new(
        manifest.contract.clone(),
        store.clone(),
        objects.clone(),
        ingestion.clone(),
        client.clone(),
        scratch.path(),
    )
    .unwrap();
    dispatch_range(&sink, &range, &DeliveryAttempt::new(1).unwrap()).await.unwrap();
    let verified = verify(&sink, &range, VerificationBudget::default()).await.unwrap();
    let publication = sink.publish(&verified, &DeliveryAttempt::new(2).unwrap()).await.unwrap();
    assert!(sink.publication_visible(&publication, &range).await.unwrap());
    drop(sink);
    drop(store);
    std::fs::remove_file(path).unwrap();
    let restored = Arc::new(ObjectRecoveryStore::open(objects.clone(), manifest.contract.clone()).await.unwrap());
    assert_eq!(restored.load(&manifest.range_id).await.unwrap(), manifest);
    let range = reopen(restored.as_ref(), &manifest.range_id).await.unwrap();
    let resumed = culprit_store_clickhouse::sink::new(
        manifest.contract.clone(),
        restored,
        objects,
        ingestion,
        client,
        scratch.path(),
    )
    .unwrap();
    verify(&resumed, &range, VerificationBudget::default()).await.unwrap();
    assert!(resumed.publication_visible(&publication, &range).await.unwrap());
}

#[tokio::test]
async fn cancelled_upload_keeps_its_native_worker_and_open_file_owned_until_completion() {
    use std::io::Write;
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (accepted, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let request = crate::http_fixture::request(&mut socket).unwrap();
        accepted.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        request
    });
    let transport = Arc::new(transport_at(port));
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"owned-native-payload").unwrap();
    let path = file.path().to_path_buf();
    let upload = {
        let transport = transport.clone();
        tokio::spawn(async move { transport.put_file("payload", &path, 20).await })
    };
    ready.await.unwrap();
    upload.abort();
    assert!(upload.await.is_err_and(|error| error.is_cancelled()));
    drop(file);
    assert_eq!(transport.requests.available_permits(), MAX_PARALLEL_REQUESTS - 1);
    release.send(()).unwrap();
    let permit =
        tokio::time::timeout(Duration::from_secs(5), transport.requests.acquire_many(MAX_PARALLEL_REQUESTS as u32))
            .await
            .unwrap()
            .unwrap();
    drop(permit);
    assert!(server.join().unwrap().ends_with(b"owned-native-payload"));
}
