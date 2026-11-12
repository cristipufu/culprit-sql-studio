use super::*;
use serde_json::json;
use std::{io::Write, net::TcpListener};
fn config(endpoint: String) -> ClickHouseConfig {
    ClickHouseConfig { endpoint, database: "fixture".into(), insert_quorum: None }
}
fn credentials() -> Credentials {
    Credentials::new("fixture-user".into(), Zeroizing::new("private-header-canary".into())).unwrap()
}
#[test]
fn configuration_is_canonical_and_cannot_hide_credentials_or_remote_plaintext() {
    for endpoint in [
        "http://warehouse.example:8123",
        "https://user:password@warehouse.example",
        "https://warehouse.example/path",
        "https://warehouse.example/?token=canary",
        "https://warehouse.example/#fragment",
        "https://warehouse.example:0",
        "https://warehouse.example:bad",
        "https://warehouse.example:65536",
        "https://warehouse.example:",
    ] {
        assert!(ClickHouseClient::new(config(endpoint.into()), credentials()).is_err(), "{endpoint}");
    }
    assert_eq!(
        config("https://WAREHOUSE.example:443/".into()).destination_id().unwrap(),
        config("https://warehouse.example".into()).destination_id().unwrap()
    );
    assert_ne!(
        config("https://one.example".into()).destination_id().unwrap(),
        config("https://two.example".into()).destination_id().unwrap()
    );
    assert!(Credentials::new("a\r\nx-secret: bad".into(), Zeroizing::new(String::new())).is_err());
}
fn server(body: Vec<u8>, status: u16) -> (String, std::thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let bytes = crate::http_fixture::request(&mut socket).unwrap();
        write!(socket, "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        socket.write_all(&body).unwrap();
        bytes
    });
    (format!("http://{address}"), worker)
}
#[tokio::test]
async fn queries_are_bounded_readonly_and_values_and_credentials_stay_out_of_sql() {
    let body =
        serde_json::to_vec(&json!({"meta":[{"name":"Value","type":"String"}],"data":[["ok"]],"rows":1})).unwrap();
    let (endpoint, worker) = server(body, 200);
    let client = ClickHouseClient::new(config(endpoint), credentials()).unwrap();
    let response = client
        .query(QueryRequest {
            text: "SELECT {value:String} AS Value".into(),
            parameters: BTreeMap::from([("value".into(), "quote'\t\\canary".into())]),
            columns: vec![Column { name: "Value".into(), kind: "String".into() }],
            row_limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(response.rows, vec![vec![json!("ok")]]);
    let request = String::from_utf8(worker.join().unwrap()).unwrap();
    let (headers, sql) = request.split_once("\r\n\r\n").unwrap();
    assert!(headers.contains("readonly=1"));
    assert!(headers.contains("max_result_rows=10001"));
    assert!(headers.contains("cancel_http_readonly_queries_on_client_close=1"));
    assert!(headers.to_ascii_lowercase().contains("x-clickhouse-key: private-header-canary"));
    assert!(sql.starts_with("/* culprit */ SELECT"));
    assert!(!sql.contains("canary"));
}
#[tokio::test]
async fn write_acks_are_hints_and_late_errors_are_ambiguous_without_leaking_bodies() {
    for (status, reply, accepted) in [
        (200, b"".to_vec(), true),
        (200, b"Code: private-header-canary".to_vec(), false),
        (500, b"private-header-canary".to_vec(), false),
    ] {
        let (endpoint, worker) = server(reply, status);
        let client = ClickHouseClient::new(config(endpoint), credentials()).unwrap();
        let submission = client.insert("Fixture", InsertData::Control(b"{}\n".to_vec())).await.unwrap();
        assert_eq!(matches!(submission, Submission::Accepted), accepted);
        assert!(accepted || matches!(submission, Submission::Unknown));
        let request = String::from_utf8(worker.join().unwrap()).unwrap();
        assert!(request.contains("async_insert=0"));
    }
    let (endpoint, worker) = server(b"{\"private-header-canary\":true}".to_vec(), 200);
    let client = ClickHouseClient::new(config(endpoint), credentials()).unwrap();
    let error = client
        .query(QueryRequest {
            text: "SELECT 1".into(),
            parameters: BTreeMap::new(),
            columns: vec![Column { name: "n".into(), kind: "UInt8".into() }],
            row_limit: 1,
        })
        .await
        .err()
        .unwrap();
    assert!(!serde_json::to_string(&error).unwrap().contains("private-header-canary"));
    worker.join().unwrap();
}

#[tokio::test]
async fn cancelling_an_insert_keeps_its_batch_and_spool_owner_until_http_completion() {
    use culprit_collector::{
        analytical::{encoding::BatchEncoder, test_support::Recovery},
        delivery::recovery::reopen,
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (observed, received) = tokio::sync::oneshot::channel();
    let (finish, until_finished) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        crate::http_fixture::request(&mut socket).unwrap();
        observed.send(()).unwrap();
        until_finished.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let client = Arc::new(ClickHouseClient::new(config(endpoint), credentials()).unwrap());
    let catalog = Arc::new(crate::catalog::SchemaCatalog::new().unwrap());
    let recovery = Arc::new(Recovery::new(client.destination(), catalog.digest()));
    let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let encoder = BatchEncoder::new(catalog.clone(), recovery.clone(), scratch.path()).unwrap();
    let batch = Arc::new(encoder.encode(&range, &range.manifest().batches[0].batch_id).await.unwrap());
    let path = batch.path().to_owned();
    let retained = Arc::downgrade(&batch);
    drop(encoder);
    let insert = tokio::spawn(async move { client.insert("CulpritResourceSamplesV1", InsertData::Batch(batch)).await });
    received.await.unwrap();
    insert.abort();
    assert!(insert.await.unwrap_err().is_cancelled());
    assert!(retained.upgrade().is_some() && path.exists());
    assert!(
        BatchEncoder::new(catalog.clone(), recovery.clone(), scratch.path()).is_err(),
        "an unfinished upload still owns its scratch directory"
    );
    finish.send(()).unwrap();
    worker.join().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while retained.upgrade().is_some() || path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!path.exists());
    culprit_collector::analytical::test_support::released_spool(catalog, recovery, scratch.path()).await;
}
