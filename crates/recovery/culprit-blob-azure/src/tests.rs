use super::*;
use base64::{Engine, prelude::BASE64_STANDARD};
use std::io::{Read, Write};
use tokio::io::AsyncReadExt;

pub(super) fn shared_key(
    account: &str,
    key: &[u8],
    method: &str,
    path: &str,
    bytes: u64,
    condition: Option<&Condition>,
    date: &str,
) -> Zeroizing<String> {
    let mut fields = vec![
        method.to_owned(),
        String::new(),
        String::new(),
        if bytes == 0 { String::new() } else { bytes.to_string() },
        String::new(),
        if method == "PUT" { "application/octet-stream".into() } else { String::new() },
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    ];
    match condition {
        Some(Condition::Matches(etag)) => fields[8] = etag.clone(),
        Some(Condition::Absent) => fields[9] = "*".into(),
        None => (),
    }
    let blob_type = if method == "PUT" { "x-ms-blob-type:BlockBlob\n" } else { "" };
    let canonical =
        format!("{}\n{blob_type}x-ms-date:{date}\nx-ms-version:{STORAGE_VERSION}\n/{account}{path}", fields.join("\n"));
    let signature = ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key), canonical.as_bytes());
    Zeroizing::new(format!("SharedKey {account}:{}", BASE64_STANDARD.encode(signature.as_ref())))
}
fn local_transport(port: u16, prefix: &str) -> AzureBlobTransport {
    AzureBlobTransport {
        endpoint: Arc::new(Endpoint { origin: format!("http://127.0.0.1:{port}"), path_prefix: prefix.into() }),
        authorization: Authorization::Emulator { account: "culprittest".into(), key: Zeroizing::new(vec![42; 32]) },
        requests: Arc::new(Semaphore::new(MAX_PARALLEL_REQUESTS)),
    }
}
fn typed(error: culprit_core::Error) -> DeliveryProblem {
    let Some(culprit_core::ErrorDetail::Delivery(problem)) = error.detail else { panic!("typed delivery failure") };
    problem
}
fn server(response: Vec<u8>) -> (u16, std::thread::JoinHandle<String>) {
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

#[test]
fn production_endpoint_cannot_redirect_credentials_to_an_arbitrary_host_or_path() {
    let config = AzureBlobConfig {
        account: "myaccount".into(),
        container: "private-recovery".into(),
        prefix: "pipeline/shard-0".into(),
    };
    let endpoint = config.endpoint().unwrap();
    assert_eq!(endpoint.origin, "https://myaccount.blob.core.windows.net");
    assert_eq!(endpoint.path("manifests/abc.json").unwrap(), "/private-recovery/pipeline/shard-0/manifests/abc.json");
    for account in ["x", "account.example.com", "a@evil", "CAPITAL", "account/other"] {
        let mut value = config.clone();
        value.account = account.into();
        assert!(value.endpoint().is_err());
    }
    for key in ["", "../outside", "dir/../outside", "dir//name", "/absolute", "x?sig=secret", "x%2fy", "x#y", "x\\y"] {
        assert!(endpoint.path(key).is_err(), "{key}");
    }
    for container in ["ab", "-prefix", "suffix-", "two--dashes", "x/y"] {
        let mut value = config.clone();
        value.container = container.into();
        assert!(value.endpoint().is_err());
    }
}

#[tokio::test]
async fn download_streams_exact_bytes_and_pins_the_recovery_namespace() {
    let (port, task) =
        server(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nETag: \"one\"\r\nConnection: close\r\n\r\nhello".to_vec());
    let transport = local_transport(port, "/culprittest/recovery/pipeline");
    let mut object = transport.get("payloads/test.ndjson").await.unwrap().unwrap();
    assert_eq!(object.bytes, 5);
    assert_eq!(object.etag, "\"one\"");
    let mut bytes = Vec::new();
    object.body.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, b"hello");
    let request = task.join().unwrap();
    assert!(request.starts_with("GET /culprittest/recovery/pipeline/payloads/test.ndjson HTTP/1.1\r\n"));
    assert!(request.to_ascii_lowercase().contains("accept-encoding: identity"));
}

#[tokio::test]
async fn missing_truncated_partial_redirected_and_oversized_downloads_are_distinct() {
    let (port, task) = server(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec());
    assert!(local_transport(port, "/test").get("missing").await.unwrap().is_none());
    task.join().unwrap();
    let (port, task) =
        server(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nETag: \"one\"\r\nConnection: close\r\n\r\nshort".to_vec());
    let mut value = local_transport(port, "/test").get("short").await.unwrap().unwrap();
    assert!(value.body.read_to_end(&mut vec![]).await.is_err());
    task.join().unwrap();
    for response in [
        "HTTP/1.1 206 Partial Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_owned(),
        "HTTP/1.1 200 OK\r\nETag: \"one\"\r\nContent-Length: 1073741825\r\nConnection: close\r\n\r\n".to_owned(),
        "HTTP/1.1 200 OK\r\nETag: \"one\"\r\nContent-Length: 0\r\nContent-Encoding: gzip\r\nConnection: close\r\n\r\n"
            .to_owned(),
    ] {
        let (port, task) = server(response.into_bytes());
        assert!(local_transport(port, "/test").get("bad").await.is_err());
        task.join().unwrap();
    }
}

#[tokio::test]
async fn lost_worker_or_late_read_error_cannot_appear_as_successful_eof() {
    let (sender, receiver) = mpsc::channel(2);
    sender.send(Ok(b"all bytes".to_vec())).await.unwrap();
    drop(sender);
    let mut reader = reader::DownloadReader::new(receiver);
    assert!(reader.read_to_end(&mut vec![]).await.is_err());
    struct LateError(bool);
    impl Read for LateError {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.0 {
                return Err(std::io::ErrorKind::ConnectionReset.into());
            }
            self.0 = true;
            bytes[..4].copy_from_slice(b"full");
            Ok(4)
        }
    }
    let (sender, receiver) = mpsc::channel(2);
    let task = tokio::task::spawn_blocking(move || reader::stream(LateError(false), 4, sender));
    let mut reader = reader::DownloadReader::new(receiver);
    let mut bytes = Vec::new();
    assert!(reader.read_to_end(&mut bytes).await.is_err());
    assert_eq!(bytes, b"full");
    task.await.unwrap();
}

#[tokio::test]
async fn dropping_a_download_releases_the_blocked_worker_and_its_admission_permit() {
    let requests = Arc::new(Semaphore::new(1));
    let permit = requests.clone().acquire_owned().await.unwrap();
    let (sender, receiver) = mpsc::channel(2);
    let worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        reader::stream(std::io::repeat(b'x'), MAX_RANGE_BYTES, sender);
    });
    let mut reader = reader::DownloadReader::new(receiver);
    let mut bytes = vec![0; DOWNLOAD_CHUNK_BYTES];
    reader.read_exact(&mut bytes).await.unwrap();
    assert_eq!(requests.available_permits(), 0);
    drop(reader);
    tokio::time::timeout(Duration::from_secs(2), worker).await.unwrap().unwrap();
    assert_eq!(requests.available_permits(), 1);
}

#[tokio::test]
async fn conditional_writes_do_not_turn_ambiguous_failures_into_acknowledgements() {
    for (status, condition, expected_header) in [
        (201, WriteCondition::Absent, "if-none-match: *"),
        (412, WriteCondition::Matches("\"old\""), "if-match: \"old\""),
    ] {
        let (port, task) =
            server(format!("HTTP/1.1 {status} Test\r\nx-ms-error-code: ConditionNotMet\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes());
        let reply = local_transport(port, "/test").put("recovery.json", b"{}".to_vec(), condition).await.unwrap();
        assert_eq!(matches!(reply, ObjectWrite::Written), status == 201);
        assert!(task.join().unwrap().to_ascii_lowercase().contains(expected_header));
    }
    let (port, task) =
        server(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret".to_vec());
    let error =
        local_transport(port, "/test").put("recovery.json", vec![], WriteCondition::Absent).await.err().unwrap();
    assert_eq!(typed(error.clone()), DeliveryProblem::Transport);
    assert!(!format!("{error:?}").contains("secret"));
    task.join().unwrap();
    for (code, conflict) in [("BlobAlreadyExists", true), ("LeaseIdMissing", false)] {
        let response = format!(
            "HTTP/1.1 409 Conflict\r\nx-ms-error-code: {code}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let (port, task) = server(response.into_bytes());
        let result = local_transport(port, "/test").put("payload", vec![], WriteCondition::Absent).await;
        assert_eq!(matches!(result, Ok(ObjectWrite::ConditionNotMet)), conflict);
        task.join().unwrap();
    }
}

#[tokio::test]
#[ignore = "requires the owned loopback Azurite fixture; CULPRIT_TEST_AZURITE_PORT"]
async fn azurite_conditional_checkpoint_and_streamed_payload_roundtrip() {
    let port = std::env::var("CULPRIT_TEST_AZURITE_PORT").expect("owned fixture port").parse::<u16>().unwrap();
    let transport = Arc::new(local_transport(port, "/culprittest/recovery/roundtrip"));
    culprit_collector::delivery::object_store::test_support::verify_recovery_contract(transport).await;
}
