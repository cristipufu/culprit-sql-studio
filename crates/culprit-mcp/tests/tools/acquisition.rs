use super::*;
use culprit_core::{Error, ErrorCode, ErrorDetail, ProviderReadContext, telemetry::*};
/// Controlled native-owner result; no real connection, SQL or native cost authority is asserted.
struct ObservedReplay {
    inner: Replay,
    context: ProviderReadContext,
    unconfirmed: bool,
    fail_finalization: Arc<std::sync::Mutex<Option<PathBuf>>>,
}
#[async_trait]
impl Provider for ObservedReplay {
    fn engine(&self) -> Engine {
        Engine::Mssql
    }
    fn source_shutdown(&self) -> SourceShutdown {
        if self.unconfirmed { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent }
    }
    async fn probe(&self) -> Result<ServerInfo> {
        self.inner.probe().await
    }
    async fn snapshot(&self, name: &str, options: &SnapshotOptions) -> Result<Snapshot> {
        let reader = self.context.reader(Engine::Mssql);
        reader.setup_included();
        let metrics = ReadMetricsRecorder::new(Engine::Mssql);
        let id = metrics.begin("SELECT N'private-mcp-template-9264'");
        metrics.finish(id.as_deref(), Duration::from_millis(1), &ReadProgress::default(), &ReadTaskOutcome::default());
        reader.checkpoint(1, metrics.snapshot());
        reader.finish(SourceShutdown::Quiescent);
        if self.unconfirmed {
            return Err(Error::new(ErrorCode::QueryFailed, "original-reader-failure")
                .with_detail(ErrorDetail::CollectionStopped(CollectionStop::ByteLimit)));
        }
        self.inner.snapshot(name, options).await
    }
    async fn recorded_changes(&self, _: &str) -> Result<Vec<culprit_core::ChangeEvent>> {
        if let Some(path) = self.fail_finalization.lock().unwrap().take() {
            std::fs::create_dir(path).unwrap();
        }
        Ok(vec![])
    }
    async fn explain(&self, sql: &str, mode: ExplainMode) -> Result<Plan> {
        self.inner.explain(sql, mode).await
    }
    async fn live(&self) -> Result<LiveState> {
        self.inner.live().await
    }
    async fn run(&self, sql: &str, rows: usize) -> Result<ResultSet> {
        self.inner.run(sql, rows).await
    }
}
fn observed_fixture(tag: &str, unconfirmed: bool) -> Fixture {
    observed_fixture_with_finalization(tag, unconfirmed, false)
}
fn observed_fixture_with_finalization(tag: &str, unconfirmed: bool, fail: bool) -> Fixture {
    let replay = Arc::new(report());
    let failure = Arc::new(std::sync::Mutex::new(None));
    let fail_finalization = failure.clone();
    let factory: culprit_engine::ProviderFactory = Arc::new(move |_, context| {
        context.complete_reader_coverage();
        Ok(Box::new(ObservedReplay {
            inner: Replay { report: replay.clone() },
            context,
            unconfirmed,
            fail_finalization: fail_finalization.clone(),
        }))
    });
    let fixture = fixture_with_factory(tag, Some(factory));
    if fail {
        *failure.lock().unwrap() = Some(fixture.dir.join("collection-admission").join(".state.json.new"));
    }
    fixture
}

#[tokio::test]
async fn successful_scan_returns_one_current_projection_and_preserves_ordinary_tools() {
    let f = observed_fixture("current-acquisition-success", false);
    let s = server(&f, Options::default());
    let (failed, body) = call(&s, "scan", json!({"connection":"replay-demo"})).await;
    assert!(!failed, "{body}");
    assert_eq!(body["provider_acquisition"]["kind"], "available");
    let evidence = &body["provider_acquisition"]["evidence"];
    assert_eq!(evidence["native_disposition"], "quiescent");
    assert_eq!(evidence["readers"][0]["queries"], 1);
    assert!(evidence.get("capture").is_none());
    assert!(evidence.get("database_identity").is_none());
    assert!(!evidence.to_string().contains("private-mcp-template-9264"));
    let (failed, ordinary) = call(&s, "list_connections", json!({})).await;
    assert!(!failed);
    assert!(ordinary.get("provider_acquisition").is_none());
    let audit = f.ws.audit_log(10).unwrap();
    assert_eq!(audit.iter().find(|entry| entry.tool == "scan").unwrap().outcome, AuditOutcome::Ok);
}
#[tokio::test]
async fn failed_native_settlement_returns_original_error_separately_and_fences_replacement_scan() {
    let f = observed_fixture("current-acquisition-failure", true);
    let s = server(&f, Options::default());
    let (failed, body) = call(&s, "scan", json!({"connection":"replay-demo"})).await;
    assert!(failed);
    assert_eq!(body["error"]["detail"], json!({"collection-stopped":"cancellation-unconfirmed"}));
    assert_eq!(body["original_error"]["message"], "original-reader-failure");
    assert_eq!(body["original_error"]["detail"], json!({"collection-stopped":"byte-limit"}));
    assert_eq!(body["provider_acquisition"]["evidence"]["native_disposition"], "unconfirmed");
    assert_eq!(body["provider_acquisition"]["evidence"]["readers"][0]["queries"], 1);
    assert!(!body["provider_acquisition"].to_string().contains("private-mcp-template-9264"));
    assert!(f.ws.collection_admission_status().await.unwrap().source_quiescence_required);
    let (failed, replacement) = call(&s, "scan", json!({"connection":"replay-demo"})).await;
    assert!(failed);
    assert_eq!(replacement["provider_acquisition"]["evidence"]["native_disposition"], "not-started");
    assert!(
        f.ws.audit_log(10)
            .unwrap()
            .iter()
            .filter(|entry| entry.tool == "scan")
            .all(|entry| entry.outcome == AuditOutcome::Error)
    );
}

#[tokio::test]
async fn successful_acquisition_with_failed_finalization_does_not_publish_an_original_error_or_a_report() {
    let f = observed_fixture_with_finalization("current-acquisition-finalization-only", false, true);
    let before = f.ws.report_history("replay-demo").unwrap();
    let s = server(&f, Options::default());
    let (failed, body) = call(&s, "scan", json!({"connection":"replay-demo"})).await;
    assert!(failed);
    assert_eq!(body["error"]["detail"], json!({"collection-stopped":"storage-failed"}));
    assert!(body.get("original_error").is_none());
    let evidence = &body["provider_acquisition"]["evidence"];
    assert_eq!(evidence["original_failure"], Value::Null);
    assert_eq!(evidence["native_disposition"], "quiescent");
    assert_eq!(evidence["finalization_failure"]["stop"], "storage-failed");
    assert_eq!(evidence["readers"][0]["queries"], 1);
    let after = f.ws.report_history("replay-demo").unwrap();
    assert_eq!(serde_json::to_value(&after).unwrap(), serde_json::to_value(&before).unwrap());
    assert!(f.ws.collection_admission_status().await.unwrap().source_quiescence_required);
    assert_eq!(
        f.ws.audit_log(10).unwrap().iter().find(|entry| entry.tool == "scan").unwrap().outcome,
        AuditOutcome::Error
    );
}
