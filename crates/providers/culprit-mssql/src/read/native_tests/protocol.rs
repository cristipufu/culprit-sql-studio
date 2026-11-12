//! Inspect only post-login fixture packets. Never trace authentication or a non-loopback endpoint.
use super::*;
use futures_util::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Default)]
struct Trace {
    armed: AtomicBool,
    packets: Mutex<[Vec<u8>; 2]>,
}
async fn relay(
    mut input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    trace: Arc<Trace>,
    direction: usize,
) -> std::io::Result<()> {
    let mut buffer = [0_u8; 4096];
    loop {
        let count = input.read(&mut buffer).await?;
        if count == 0 {
            return output.close().await;
        }
        if trace.armed.load(Ordering::Acquire) {
            let mut packets = trace.packets.lock().unwrap();
            let remaining = (64 * 1024_usize).saturating_sub(packets[direction].len());
            packets[direction].extend_from_slice(&buffer[..count.min(remaining)]);
        }
        output.write_all(&buffer[..count]).await?;
    }
}
fn describe_packets(bytes: &[u8]) -> Vec<String> {
    let mut packets = vec![];
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let size = usize::from(u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]));
        if size < 8 || offset + size > bytes.len() {
            break;
        }
        let payload =
            bytes[offset + 8..offset + size].iter().take(256).map(|byte| format!("{byte:02x}")).collect::<String>();
        packets.push(format!(
            "type={:02x},status={:02x},size={size},payload={payload}",
            bytes[offset],
            bytes[offset + 1]
        ));
        offset += size;
    }
    packets
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn cancellation_protocol_trace_records_post_login_response_boundaries() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    let trace = Arc::new(Trace::default());
    let traffic = trace.clone();
    let proxy = tokio::spawn(async move {
        let (client, _) = listener.accept().await.unwrap();
        let server = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await.unwrap();
        let (client_in, client_out) = client.into_split();
        let (server_in, server_out) = server.into_split();
        tokio::join!(
            relay(client_in.compat(), server_out.compat_write(), traffic.clone(), 0),
            relay(server_in.compat(), client_out.compat_write(), traffic, 1)
        )
    });
    let pool = Arc::new(Pool::new(config(proxy_port, "master", &password, "culprit"), None));
    let mut reader = ReadSession::new(pool, CollectionBudget::default());
    let session = identity(&mut reader).await;
    let direct = Pool::new(config(port, "master", &password, "culprit-observer-fixture"), None);
    let mut observer = direct.connect(SessionKind::User).await.unwrap();
    assert!(reader.client.is_some(), "the trace must reuse the authenticated connection");
    reader.budget.statement_timeout_ms = 1500;
    trace.armed.store(true, Ordering::Release);
    let result=reader.query("SELECT SUM(CONVERT(bigint,CHECKSUM(NEWID()))) AS n FROM sys.all_objects a CROSS JOIN sys.all_objects b CROSS JOIN sys.all_objects c OPTION(MAXDOP 1)",vec![],1).await;
    let shutdown = reader.close().await;
    assert_quiet(&mut observer, &session).await;
    let mut proxy = proxy;
    if tokio::time::timeout(Duration::from_secs(3), &mut proxy).await.is_err() {
        proxy.abort();
        let _ = proxy.await;
    }
    trace.armed.store(false, Ordering::Release);
    let packets = trace.packets.lock().unwrap();
    for (direction, bytes) in ["client", "server"].into_iter().zip(packets.iter()) {
        for packet in describe_packets(bytes) {
            eprintln!("Post-login cancellation fixture {direction}: {packet}");
        }
    }
    eprintln!("Post-login cancellation fixture result={result:?},shutdown={shutdown:?}");
    assert!(matches!(
        result,
        Err(ReadError::Stopped(CollectionStop::TimedOut | CollectionStop::CancellationUnconfirmed))
    ));
    if shutdown == SourceShutdown::Unconfirmed {
        assert!(matches!(reader.admit(), Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))));
    }
}
