//! Minimal bounded probe server. No database, destination, credential or filesystem calls on request paths.
use super::CollectorHealth;
use crate::capture::CollectionCancellation;
use culprit_core::{Result, telemetry::DeliveryProblem};
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};

const MAX_CONNECTIONS: usize = 8;
const MAX_REQUEST_BYTES: usize = 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
pub struct HealthServer {
    listener: TcpListener,
    health: CollectorHealth,
}
impl HealthServer {
    pub async fn bind(address: SocketAddr, health: CollectorHealth) -> Result<Self> {
        Ok(Self { listener: TcpListener::bind(address).await.map_err(|_| failed())?, health })
    }
    pub async fn run(self, cancellation: &CollectionCancellation) -> Result<()> {
        let Self { listener, health } = self;
        let mut tasks = JoinSet::new();
        let result = loop {
            tokio::select! {
                biased;
                _=cancellation.cancelled()=>break Ok(()),
                result=tasks.join_next(),if !tasks.is_empty()=>{
                    if result.is_some_and(|r|r.is_err()){break Err(failed());}
                },
                accepted=listener.accept(),if tasks.len()<MAX_CONNECTIONS=>{
                    let (socket,_)=match accepted {Ok(pair)=>pair,Err(_)=>break Err(failed())};
                    let health=health.clone();
                    tasks.spawn(async move{let _=reply(socket,health).await;});
                },
            }
        };
        drop(listener);
        // Every accepted socket has a deadline. Shutdown retains ownership until these tasks end.
        while tasks.join_next().await.is_some() {}
        result
    }
}
async fn reply(mut socket: TcpStream, health: CollectorHealth) -> Result<()> {
    let Some(request) =
        tokio::time::timeout(REQUEST_TIMEOUT, read_request(&mut socket)).await.map_err(|_| failed())??
    else {
        return Ok(());
    };
    let (status, content_type, body) = response(&request, health).await?;
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        socket.write_all(headers.as_bytes()).await.map_err(|_| failed())?;
        socket.write_all(&body).await.map_err(|_| failed())?;
        socket.shutdown().await.map_err(|_| failed())
    })
    .await
    .map_err(|_| failed())?
}
async fn read_request(socket: &mut TcpStream) -> Result<Option<String>> {
    let mut bytes = [0u8; MAX_REQUEST_BYTES];
    let mut used = 0;
    while used < bytes.len() && !bytes[..used].windows(4).any(|w| w == b"\r\n\r\n") {
        let count = socket.read(&mut bytes[used..]).await.map_err(|_| failed())?;
        if count == 0 {
            return Ok(None);
        }
        used += count;
    }
    if !bytes[..used].windows(4).any(|w| w == b"\r\n\r\n") {
        return Ok(Some(String::new()));
    }
    let line = bytes[..used].split(|b| *b == b'\n').next().unwrap_or(&[]);
    Ok(Some(std::str::from_utf8(line).ok().and_then(|s| s.strip_suffix('\r')).unwrap_or("").into()))
}
async fn response(request: &str, health: CollectorHealth) -> Result<(&'static str, &'static str, Vec<u8>)> {
    Ok(match request {
        "GET /live HTTP/1.1" | "GET /live HTTP/1.0" => ("200 OK", "text/plain", b"live\n".to_vec()),
        "GET /ready HTTP/1.1" | "GET /ready HTTP/1.0" => {
            let ready = health.ready()?;
            if ready {
                ("200 OK", "text/plain", b"ready\n".to_vec())
            } else {
                ("503 Service Unavailable", "text/plain", b"not ready\n".to_vec())
            }
        }
        "GET /metrics HTTP/1.1" | "GET /metrics HTTP/1.0" => {
            // Keep the request task and its capacity slot until formatting finishes. A network deadline
            // cannot retire an owned CPU worker or put metrics work on the native cancellation executor.
            let reply = tokio::task::spawn_blocking(move || health.reply()).await.map_err(|_| failed())??;
            ("200 OK", "text/plain; version=0.0.4", reply.metrics)
        }
        _ => ("404 Not Found", "text/plain", b"not found\n".to_vec()),
    })
}
fn failed() -> culprit_core::Error {
    DeliveryProblem::Storage.error()
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn get(address: SocketAddr, path: &str) -> String {
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).await.unwrap();
        let mut bytes = vec![];
        socket.read_to_end(&mut bytes).await.unwrap();
        String::from_utf8(bytes).unwrap()
    }
    #[tokio::test]
    async fn liveness_survives_initialization_or_unconfirmed_native_work_and_shutdown_drains_accepted_connections() {
        let health = crate::health::tests::health();
        let server = HealthServer::bind("127.0.0.1:0".parse().unwrap(), health.clone()).await.unwrap();
        let address = server.listener.local_addr().unwrap();
        let cancellation = CollectionCancellation::default();
        let ending = cancellation.clone();
        let worker = tokio::spawn(async move { server.run(&ending).await });
        assert!(get(address, "/live").await.starts_with("HTTP/1.1 200"));
        assert!(get(address, "/ready").await.starts_with("HTTP/1.1 503"));
        health.initialized().unwrap();
        health.outbox(&crate::health::tests::status(0)).unwrap();
        assert!(get(address, "/ready").await.starts_with("HTTP/1.1 200"));
        health.outbox(&crate::health::tests::status(1)).unwrap();
        assert!(get(address, "/live").await.starts_with("HTTP/1.1 200"));
        assert!(get(address, "/ready").await.starts_with("HTTP/1.1 503"));
        let metrics = get(address, "/metrics").await;
        assert!(metrics.contains("culprit_collector_outbox_source_debts 1"));
        assert!(!metrics.contains("private-connection-canary"));
        assert!(get(address, "/not-a-route").await.starts_with("HTTP/1.1 404"));
        cancellation.cancel();
        tokio::time::timeout(REQUEST_TIMEOUT * 2, worker).await.unwrap().unwrap().unwrap();
        // The joined owner has dropped its listener. Probing its old port races another test acquiring that port.
    }
}
