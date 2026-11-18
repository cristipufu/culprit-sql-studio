use super::*;
use culprit_core::{AuthKind, Engine, ErrorDetail};
use culprit_engine::{AgentAccess, LocalCaptureReader, LocalCaptureTask, NewConnection, Workspace};
use rmcp::ServerHandler;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::{Notify, oneshot};

struct Host {
    server: CulpritMcp,
    directory: PathBuf,
}
impl Host {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "culprit-local-capture-mcp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let workspace = Arc::new(Workspace::open_in(&directory));
        workspace
            .add_connection(NewConnection {
                name: "local-evidence".into(),
                input: "Server=localhost,14333;Database=fixture".into(),
                engine: Some(Engine::Mssql),
                auth: Some(AuthKind::None),
                ..Default::default()
            })
            .unwrap();
        Self { server: CulpritMcp::new(workspace, crate::Options::default()), directory }
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn arguments() -> Value {
    json!({"connection":"local-evidence", "observation_id":"a".repeat(32), "analysis":"page_server_reads"})
}
fn body(result: &rmcp::model::CallToolResult) -> Value {
    serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap()
}

struct RefusedReader(AtomicUsize);
impl LocalCaptureReader for RefusedReader {
    fn timeout(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn analyze<'a>(&'a self, _: LocalCaptureRequest<'a>) -> LocalCaptureTask<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(DeliveryProblem::Integrity.error()) })
    }
}

#[test]
fn the_offline_tool_is_absent_by_default_and_has_no_path_or_sql_arguments_when_explicitly_installed() {
    let mut host = Host::new();
    assert!(host.server.get_tool("analyze_capture").is_none());
    host.server = host.server.clone().with_local_captures(Arc::new(RefusedReader(AtomicUsize::new(0))));
    assert!(
        host.server.get_tool("captured_page_server_reads").is_none(),
        "there is no alias for the replaced operation"
    );
    let tool = culprit_core::i18n::scope("fr", || host.server.get_tool("analyze_capture").unwrap());
    let title = culprit_core::i18n::scope("en", || culprit_core::t!("mcp.local_capture.analyze.title"));
    assert_eq!(tool.title.as_deref(), Some(title.as_str()));
    let description =
        culprit_core::i18n::scope("en", || culprit_core::t!("mcp.local_capture.analyze.snapshot_description"));
    assert!(tool.description.as_deref().unwrap().starts_with(&description));
    assert_eq!(tool.annotations.as_ref().and_then(|value| value.read_only_hint), Some(true));
    assert_eq!(tool.annotations.as_ref().and_then(|value| value.open_world_hint), Some(false));
    let properties = tool.input_schema.get("properties").unwrap().as_object().unwrap();
    assert_eq!(properties.len(), 3);
    assert_eq!(properties["analysis"]["enum"], json!(LocalCaptureAnalysisKind::ALL));
    assert!(properties.contains_key("connection") && properties.contains_key("observation_id"));
    assert_eq!(tool.input_schema.get("additionalProperties"), Some(&json!(false)));
}

#[tokio::test]
async fn delegation_and_closed_arguments_are_checked_before_the_local_reader_is_invoked() {
    let mut host = Host::new();
    let reader = Arc::new(RefusedReader(AtomicUsize::new(0)));
    host.server = host.server.clone().with_local_captures(reader.clone());
    host.server.ws.set_agent_access("local-evidence", AgentAccess::Off).unwrap();
    let result = culprit_core::i18n::scope_async(
        "fr",
        host.server.handle("fixture", "analyze_capture", arguments().as_object().unwrap().clone()),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(reader.0.load(Ordering::SeqCst), 0);
    host.server.ws.set_agent_access("local-evidence", AgentAccess::Stats).unwrap();
    for extra in ["directory", "sql", "config", "with_recovery_payloads"] {
        let mut args = arguments();
        args[extra] = json!("private-path-or-query");
        let result = host.server.handle("fixture", "analyze_capture", args.as_object().unwrap().clone()).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(reader.0.load(Ordering::SeqCst), 0, "{extra}");
    }
    for analysis in [None, Some("unknown"), Some("memory-clerks")] {
        let mut invalid = arguments();
        if let Some(analysis) = analysis {
            invalid["analysis"] = json!(analysis);
        } else {
            invalid.as_object_mut().unwrap().remove("analysis");
        }
        let result = host.server.handle("fixture", "analyze_capture", invalid.as_object().unwrap().clone()).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(reader.0.load(Ordering::SeqCst), 0);
    }
    let result = culprit_core::i18n::scope_async(
        "fr",
        host.server.handle("fixture", "analyze_capture", arguments().as_object().unwrap().clone()),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(reader.0.load(Ordering::SeqCst), 1);
    assert_eq!(body(&result)["error"]["detail"]["delivery"], json!("integrity"));
    assert_eq!(
        body(&result)["error"]["message"],
        culprit_core::i18n::scope("en", || DeliveryProblem::Integrity.error().message)
    );
    let audit = host.server.ws.audit_log(1).unwrap();
    assert!(audit[0].sql.is_none());
}

struct AwaitedReader {
    started: Notify,
    release: Notify,
    settled: AtomicUsize,
}
impl LocalCaptureReader for AwaitedReader {
    fn timeout(&self) -> Duration {
        Duration::from_secs(5)
    }
    fn analyze<'a>(&'a self, request: LocalCaptureRequest<'a>) -> LocalCaptureTask<'a> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
            assert!(request.operation.is_cancelled(), "RPC cancellation reaches the same owner handle");
            self.settled.fetch_add(1, Ordering::SeqCst);
            Err(CollectionStop::Cancelled.error())
        })
    }
}

#[tokio::test]
async fn rpc_cancellation_waits_for_the_started_owner_and_preserves_typed_cancellation() {
    let host = Host::new();
    let reader =
        Arc::new(AwaitedReader { started: Notify::new(), release: Notify::new(), settled: AtomicUsize::new(0) });
    let server = host.server.clone().with_local_captures(reader.clone());
    let (cancel, cancelled) = oneshot::channel();
    let running = tokio::spawn(async move {
        server
            .handle_cancellable("fixture", "analyze_capture", arguments().as_object().unwrap().clone(), async {
                let _ = cancelled.await;
            })
            .await
    });
    reader.started.notified().await;
    cancel.send(()).unwrap();
    tokio::task::yield_now().await;
    assert!(!running.is_finished(), "the RPC cannot return while its started owner still holds work");
    assert_eq!(reader.settled.load(Ordering::SeqCst), 0);
    reader.release.notify_one();
    let result = running.await.unwrap();
    assert_eq!(reader.settled.load(Ordering::SeqCst), 1);
    assert_eq!(result.is_error, Some(true));
    assert_eq!(body(&result)["error"]["detail"]["collection-stopped"], json!("cancelled"));
}

#[test]
fn deadline_and_cancellation_remain_distinct_without_any_native_or_store_access() {
    let guard = culprit_core::limits::OperationGuard::new(Duration::ZERO).unwrap();
    assert_eq!(
        check_operation(&guard.handle()).unwrap_err().detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut))
    );
    let guard = culprit_core::limits::OperationGuard::new(Duration::from_secs(1)).unwrap();
    let operation = guard.handle();
    drop(guard);
    assert_eq!(
        check_operation(&operation).unwrap_err().detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled))
    );
}
