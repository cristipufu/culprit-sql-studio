use async_trait::async_trait;
use culprit_collector::capture::CollectionCancellation;
use culprit_core::telemetry::{
    MeasurementValue, NativeDefinitionReference, NativeIdentity, RuntimeMetric, RuntimeMetricKey, RuntimeMetricUnit,
    SourceWorkState, StatementReferences,
};
use culprit_core::{
    AuthKind, DatabaseIdentity, Engine, Error, ErrorCode, ExplainMode, LiveState, Plan, Provider, Result, ResultSet,
    ServerInfo, Snapshot, SnapshotOptions,
    telemetry::{
        CollectionBudget, CollectionDescription, CollectionRead, CollectionRequest, CollectionStatus, CollectionStop,
        DefinitionPayload, NativeMeasurementKey, NativePlanIdentity, NativeQueryIdentity, PlanDefinition,
        QueryDefinition, RuntimeMeasurement, SourceContinuity, TelemetryInterval, TelemetryRecord, TelemetrySource,
        TelemetryStream,
    },
};
use culprit_engine::{NewConnection, Workspace};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Clone, Copy)]
enum Scenario {
    Success,
    Empty,
    LateFailure,
    Blocked,
    Cycle,
    Duplicate,
    LargePage,
    ChangedIdentity,
    NonFinite,
    Unconfirmed,
    MetricsStillRunning,
    MalformedMetrics,
    OperationalState,
    QueryOnly,
    CumulativeStatements,
    PagedOperational,
    PagedWrongCounts,
    PagedMissingClosure,
    PagedBlockedClosure,
    OperationalSourceCountMismatch,
    Dimensions,
    MissingDefinitions,
    UnresolvedIdentity,
    UnredactedDefinitions,
    PressureAtVerification,
    ResourceHistory,
    ExpiredResourceHistory,
    DuplicateResourceSample,
    ResourceCountMismatch,
    ActivitySequence,
    ActivityConflictingRows,
    ActivityUnconfirmed,
    ActivityRestricted,
    ActivityLockPoint,
    ResourceDiscoveryDuplicate,
    ResourceDiscoveryWrongKind,
    ResourceDiscoveryWrongEngine,
    ResourceDiscoveryMalformed,
    ResourceDiscoveryOversized,
    ResourceDiscoveryDenied,
    ResourceDiscoveryUnconfirmed,
}
struct State {
    scenario: Scenario,
    entered: tokio::sync::Notify,
    active: AtomicBool,
    closed: AtomicBool,
    calls: AtomicUsize,
    factories: AtomicUsize,
    completions: AtomicUsize,
    polls: AtomicUsize,
}
impl State {
    fn new(scenario: Scenario) -> Arc<Self> {
        Arc::new(Self {
            scenario,
            entered: tokio::sync::Notify::new(),
            active: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            factories: AtomicUsize::new(0),
            completions: AtomicUsize::new(0),
            polls: AtomicUsize::new(0),
        })
    }
}
struct TestSource(Arc<State>, usize);
struct PagedStateSource {
    state: Arc<State>,
    offset: usize,
}
#[async_trait]
impl TelemetrySource for PagedStateSource {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        use culprit_core::telemetry::*;
        let mut source = operational_description(Scenario::OperationalState);
        source.observation = SourceObservation::PagedSnapshot(PagedSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T06:00:00Z".into(),
            sources: vec!["log".into()],
        });
        Ok(source)
    }
    fn query_count(&self) -> u32 {
        self.state.calls.load(Ordering::SeqCst) as u32
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        assert_eq!(stream, TelemetryStream::OperationalSamples);
        assert_eq!(cursor, if self.offset == 0 { None } else { Some("next") });
        self.state.calls.fetch_add(1, Ordering::SeqCst);
        let mut records = operational_rows(Scenario::OperationalState);
        let TelemetryRecord::Operational(row) = &mut records[0] else { panic!("operational row") };
        row.key = format!("row-{}", self.offset);
        self.offset += 1;
        Ok(CollectionRead::Page { records, next_cursor: (self.offset == 1).then(|| "next".into()) })
    }
    async fn finish(&mut self) -> Result<culprit_core::telemetry::SourceCompletion> {
        use culprit_core::telemetry::*;
        self.state.completions.fetch_add(1, Ordering::SeqCst);
        if matches!(self.state.scenario, Scenario::PagedMissingClosure) {
            return Ok(SourceCompletion::Verified);
        }
        if matches!(self.state.scenario, Scenario::PagedBlockedClosure) {
            self.state.active.store(true, Ordering::SeqCst);
            let _guard = ActiveRead(self.state.clone());
            self.state.entered.notify_one();
            return std::future::pending().await;
        }
        let records = if matches!(self.state.scenario, Scenario::PagedWrongCounts) { 1 } else { 2 };
        Ok(SourceCompletion::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T06:00:00Z".into(),
            observed_until: "2020-01-01T06:00:01Z".into(),
            records,
            sources: vec![StateSourceCoverage::collected("log".into(), records)],
        }))
    }
    async fn close(&mut self) -> culprit_core::telemetry::SourceShutdown {
        assert!(!self.state.active.load(Ordering::SeqCst));
        self.state.closed.store(true, Ordering::SeqCst);
        culprit_core::telemetry::SourceShutdown::Quiescent
    }
}
struct ActiveRead(Arc<State>);
impl Drop for ActiveRead {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::SeqCst);
    }
}

fn description() -> CollectionDescription {
    CollectionDescription {
        target: DatabaseIdentity::server(Engine::Postgres, "sample", "fixture.invalid", 5432, None).into(),
        source: "fixture-statistics".into(),
        observation: culprit_core::telemetry::SourceObservation::QueryHistory(
            culprit_core::telemetry::QueryHistoryDescription {
                waits: None,
                interval: TelemetryInterval {
                    native_id: "123".into(),
                    from: "2020-01-01T06:00:00Z".into(),
                    until: "2020-01-01T07:00:00Z".into(),
                },
                dimensions: [("replica_group_id".into(), vec!["17".into(), "18".into()])].into(),
                native_properties: [
                    ("capture_mode".into(), "ALL".into()),
                    ("waits_capture_enabled".into(), (true).to_string()),
                ]
                .into(),
            },
        ),
        continuity: SourceContinuity::Unverified,
    }
}

fn plan_identity() -> NativePlanIdentity {
    NativePlanIdentity {
        plan_id: "9007199254740993".into(),
        query: NativeQueryIdentity {
            query_id: "21".into(),
            qualifiers: [
                ("context_settings_id".into(), "2".into()),
                ("object_id".into(), "0".into()),
                ("query_hash".into(), "0123456789ABCDEF".into()),
                ("initial_compile_at".into(), "2020-01-01T05:00:00Z".into()),
            ]
            .into(),
        },
        qualifiers: [
            ("plan_hash".into(), "FEDCBA9876543210".into()),
            ("initial_compile_at".into(), "2020-01-01T05:00:01Z".into()),
        ]
        .into(),
    }
}
fn definition(stream: TelemetryStream) -> TelemetryRecord {
    let identity = plan_identity();
    let source = description();
    let query_id = identity.query.definition_id(&source).unwrap();
    if stream == TelemetryStream::QueryDefinitions {
        TelemetryRecord::QueryDefinition(QueryDefinition {
            variant_family: None,
            origin: culprit_core::telemetry::StatementOrigin::Unknown,
            monitoring_template_id: None,
            id: query_id,
            identity: identity.query,
            context: Default::default(),
            context_available: true,
            source_text_bytes: Some("100".into()),
            sql: DefinitionPayload::sql(
                "SELECT * FROM customer WHERE token = N'secret-value' /* secret-comment */",
                Engine::Mssql,
            ),
        })
    } else {
        TelemetryRecord::PlanDefinition(PlanDefinition {
            identity: identity.clone(),
            id: identity.definition_id(&source).unwrap(),
            query_definition_id: Some(query_id),
            source_text_bytes: Some("200".into()),
            plan: DefinitionPayload::plan(
                "<ShowPlanXML><StmtSimple StatementText=\"SELECT 'secret-value'\" /></ShowPlanXML>",
                Engine::Mssql,
            ),
            engine_version: Some("16.0".into()),
            native_properties: [("compatibility_level".into(), "160".to_string())].into(),
        })
    }
}

fn measurement(id: usize) -> TelemetryRecord {
    TelemetryRecord::QueryRuntime(RuntimeMeasurement {
        key: NativeMeasurementKey {
            native: NativeIdentity::new(id.to_string())
                .qualified("plan_id", "9007199254740993")
                .qualified("replica_group_id", "17")
                .qualified("execution_type", "3".to_string()),
            references: StatementReferences {
                query: Some(NativeDefinitionReference {
                    native: plan_identity().query.native(),
                    definition_id: Some(plan_identity().query.definition_id(&description()).unwrap()),
                }),
                plan: Some(NativeDefinitionReference {
                    native: plan_identity().native(),
                    definition_id: Some(plan_identity().definition_id(&description()).unwrap()),
                }),
            },
        },
        executions: "9007199254740993".into(),
        metrics: vec![
            RuntimeMetric {
                minimum: Some(1.0),
                maximum: Some(20.0),
                standard_deviation: Some(2.0),
                ..RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Microseconds, 10.0)
            },
            RuntimeMetric::average(RuntimeMetricKey::CpuTime, RuntimeMetricUnit::Microseconds, 5.0),
            RuntimeMetric::average(
                RuntimeMetricKey::LogicalReads,
                RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
                2.0,
            ),
            RuntimeMetric::average(
                RuntimeMetricKey::PhysicalReads,
                RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
                0.0,
            ),
        ],
        first_execution_at: Some("2020-01-01T06:01:00Z".into()),
        last_execution_at: Some("2020-01-01T06:59:00Z".into()),
    })
}
#[async_trait]
impl TelemetrySource for TestSource {
    fn read_metrics(&self) -> Option<culprit_core::telemetry::ReadWorkMetrics> {
        if matches!(self.0.scenario, Scenario::MalformedMetrics) {
            return Some(culprit_core::telemetry::ReadWorkMetrics {
                active_tasks: 0,
                untracked_tasks: 0,
                templates: vec![culprit_core::telemetry::ReadQueryMetrics {
                    template_id: "invalid-template-id".into(),
                    statement: DefinitionPayload::Unavailable {
                        reason: culprit_core::telemetry::PayloadUnavailable::NotCollected,
                    },
                    completed_tasks: 1,
                    failures: 0,
                    timeouts: 0,
                    cancellations: 0,
                    unconfirmed_cancellations: 0,
                    rows_decoded: "1".into(),
                    charged_value_bytes: "16".into(),
                    client_elapsed_ms: 1.0,
                }],
            });
        }
        matches!(self.0.scenario, Scenario::MetricsStillRunning).then_some(culprit_core::telemetry::ReadWorkMetrics {
            active_tasks: 1,
            untracked_tasks: 0,
            templates: vec![],
        })
    }
    async fn describe(&mut self) -> Result<CollectionDescription> {
        if matches!(
            self.0.scenario,
            Scenario::OperationalState | Scenario::OperationalSourceCountMismatch | Scenario::CumulativeStatements
        ) {
            return Ok(operational_description(self.0.scenario));
        }
        if matches!(
            self.0.scenario,
            Scenario::ActivitySequence
                | Scenario::ActivityConflictingRows
                | Scenario::ActivityUnconfirmed
                | Scenario::ActivityRestricted
                | Scenario::ActivityLockPoint
        ) {
            if matches!(self.0.scenario, Scenario::ActivitySequence) && self.1 == 1 {
                return Err(CollectionStop::SourceUnavailable.error());
            }
            return Ok(activity_description(self.0.scenario, self.1));
        }
        if matches!(
            self.0.scenario,
            Scenario::ResourceHistory
                | Scenario::ExpiredResourceHistory
                | Scenario::DuplicateResourceSample
                | Scenario::ResourceCountMismatch
        ) {
            return Ok(resource_description(self.0.scenario));
        }
        let mut source = description();
        if matches!(self.0.scenario, Scenario::QueryOnly) {
            let culprit_core::telemetry::SourceObservation::QueryHistory(history) = &mut source.observation else {
                unreachable!()
            };
            history.dimensions.clear();
            history.native_properties.clear();
        }
        Ok(source)
    }
    fn query_count(&self) -> u32 {
        self.0.calls.load(Ordering::SeqCst) as u32
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        if matches!(
            self.0.scenario,
            Scenario::OperationalState | Scenario::OperationalSourceCountMismatch | Scenario::CumulativeStatements
        ) {
            assert_eq!(stream, TelemetryStream::OperationalSamples);
            assert!(cursor.is_none());
            return Ok(CollectionRead::Page { records: operational_rows(self.0.scenario), next_cursor: None });
        }
        if matches!(self.0.scenario, Scenario::ActivityUnconfirmed) {
            return Ok(CollectionRead::Stopped(CollectionStop::CancellationUnconfirmed));
        }
        if matches!(
            self.0.scenario,
            Scenario::ActivitySequence
                | Scenario::ActivityConflictingRows
                | Scenario::ActivityUnconfirmed
                | Scenario::ActivityRestricted
                | Scenario::ActivityLockPoint
        ) {
            let rows = activity_rows(self.0.scenario, self.1);
            assert_eq!(stream, TelemetryStream::ActivityObservations, "the provider emits native baseline only");
            let records = rows.into_iter().map(culprit_core::telemetry::TelemetryRecord::from).collect();
            return Ok(CollectionRead::Page { records, next_cursor: None });
        }

        if stream == TelemetryStream::ResourceSamples {
            let mut records = vec![resource_record("2020-01-01T06:00:15Z")];
            if matches!(self.0.scenario, Scenario::DuplicateResourceSample) {
                records.push(resource_record("2020-01-01T08:00:15+02:00"));
            }
            return Ok(CollectionRead::Page { records, next_cursor: None });
        }
        if matches!(self.0.scenario, Scenario::Unconfirmed) {
            return Ok(CollectionRead::Stopped(CollectionStop::CancellationUnconfirmed));
        }
        if matches!(self.0.scenario, Scenario::Blocked) {
            self.0.active.store(true, Ordering::SeqCst);
            let _guard = ActiveRead(self.0.clone());
            self.0.entered.notify_one();
            return std::future::pending().await;
        }
        if matches!(stream, TelemetryStream::QueryDefinitions | TelemetryStream::PlanDefinitions) {
            if matches!(self.0.scenario, Scenario::MissingDefinitions | Scenario::UnresolvedIdentity) {
                return Ok(CollectionRead::Page { records: vec![], next_cursor: None });
            }
            if matches!(self.0.scenario, Scenario::QueryOnly) && stream == TelemetryStream::PlanDefinitions {
                return Ok(CollectionRead::Page { records: vec![], next_cursor: None });
            }
            let mut record = definition(stream);
            if matches!(self.0.scenario, Scenario::UnredactedDefinitions) {
                let raw = match &record {
                    TelemetryRecord::QueryDefinition(_) => "SELECT 'secret-value' /* secret-comment */",
                    _ => "<ShowPlanXML><StmtSimple StatementText=\"SELECT 'secret-value'\" /></ShowPlanXML>",
                };
                let payload = DefinitionPayload::Available {
                    text: raw.into(),
                    sha256: culprit_core::telemetry::digest(raw.as_bytes()),
                    redaction_version: 1,
                };
                match &mut record {
                    TelemetryRecord::QueryDefinition(row) => row.sql = payload,
                    TelemetryRecord::PlanDefinition(row) => row.plan = payload,
                    _ => unreachable!(),
                }
            }
            return Ok(CollectionRead::Page { records: vec![record], next_cursor: None });
        }
        if stream == TelemetryStream::QueryWaits || matches!(self.0.scenario, Scenario::Empty) {
            return Ok(CollectionRead::Page { records: vec![], next_cursor: None });
        }
        let after = cursor.is_some();
        if after && matches!(self.0.scenario, Scenario::LateFailure) {
            return Err(Error::new(ErrorCode::QueryFailed, "a late source failure"));
        }
        let mut rows = vec![measurement(if after && !matches!(self.0.scenario, Scenario::Duplicate) { 2 } else { 1 })];
        if matches!(self.0.scenario, Scenario::QueryOnly) {
            for record in &mut rows {
                let TelemetryRecord::QueryRuntime(row) = record else { unreachable!() };
                row.key.native.qualifiers.clear();
                row.key.references.plan = None;
                row.metrics.retain(|metric| metric.key == RuntimeMetricKey::Duration);
            }
        }
        if matches!(self.0.scenario, Scenario::UnresolvedIdentity)
            && let TelemetryRecord::QueryRuntime(row) = &mut rows[0]
        {
            row.key.references.plan.as_mut().unwrap().definition_id = None;
        }
        if matches!(self.0.scenario, Scenario::Dimensions) {
            let mut replica = measurement(1);
            let mut outcome = measurement(1);
            if let TelemetryRecord::QueryRuntime(row) = &mut replica {
                row.key.native.qualifiers.insert("replica_group_id".into(), "18".into());
            }
            if let TelemetryRecord::QueryRuntime(row) = &mut outcome {
                row.key.native.qualifiers.insert("execution_type".into(), "4".into());
            }
            return Ok(CollectionRead::Page { records: vec![measurement(1), replica, outcome], next_cursor: None });
        }
        if matches!(self.0.scenario, Scenario::LargePage) {
            rows = (0..4)
                .map(|id| {
                    let mut row = measurement(id);
                    if let TelemetryRecord::QueryRuntime(row) = &mut row {
                        row.key.native.qualifiers.insert("plan_id".into(), "x".repeat(128));
                    }
                    row
                })
                .collect();
        }
        if matches!(self.0.scenario, Scenario::NonFinite)
            && let TelemetryRecord::QueryRuntime(row) = &mut rows[0]
        {
            row.metrics.iter_mut().find(|metric| metric.key == RuntimeMetricKey::CpuTime).unwrap().average = f64::NAN;
        }
        Ok(CollectionRead::Page {
            records: rows,
            next_cursor: if !after || matches!(self.0.scenario, Scenario::Cycle) {
                Some("next-page".into())
            } else {
                None
            },
        })
    }
    async fn finish(&mut self) -> Result<culprit_core::telemetry::SourceCompletion> {
        if matches!(self.0.scenario, Scenario::PressureAtVerification) {
            return Err(CollectionStop::Pressure.error());
        }
        Ok(culprit_core::telemetry::SourceCompletion::verified_if(!matches!(
            self.0.scenario,
            Scenario::ChangedIdentity
        )))
    }
    async fn close(&mut self) -> culprit_core::telemetry::SourceShutdown {
        assert!(!self.0.active.load(Ordering::SeqCst), "a read must relinquish ownership before closing its source");
        tokio::task::yield_now().await;
        self.0.closed.store(true, Ordering::SeqCst);
        if matches!(self.0.scenario, Scenario::Unconfirmed | Scenario::ActivityUnconfirmed) {
            culprit_core::telemetry::SourceShutdown::Unconfirmed
        } else {
            culprit_core::telemetry::SourceShutdown::Quiescent
        }
    }
}
struct TestProvider(Arc<State>);
#[async_trait]
impl Provider for TestProvider {
    async fn telemetry_resources(
        &self,
        _: &culprit_core::telemetry::ResourceDiscoveryRequest,
    ) -> Result<Vec<culprit_core::telemetry::CollectionResource>> {
        use culprit_core::telemetry::*;
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        if matches!(self.0.scenario, Scenario::Blocked) {
            self.0.active.store(true, Ordering::SeqCst);
            let _guard = ActiveRead(self.0.clone());
            self.0.entered.notify_one();
            return std::future::pending().await;
        }
        if matches!(self.0.scenario, Scenario::Empty) {
            return Ok(vec![]);
        }
        if matches!(self.0.scenario, Scenario::ResourceDiscoveryDenied) {
            return Err(Error::new(ErrorCode::PermissionDenied, "fixture discovery permission denied"));
        }
        let mut resource = CollectionResource {
            identity: SharedResourceIdentity {
                engine: Engine::Postgres,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("native-system"),
                native: NativeIdentity::new("native-instance"),
            },
            observed_at: "2020-01-01T06:02:00Z".into(),
            collections: vec![TelemetryCollection::Snapshot(SnapshotDataset::Operational)],
        };
        match self.0.scenario {
            Scenario::ResourceDiscoveryWrongKind => resource.identity.kind = SharedResourceKind::Pool,
            Scenario::ResourceDiscoveryWrongEngine => resource.identity.engine = Engine::Mssql,
            Scenario::ResourceDiscoveryMalformed => resource.identity.native.id.clear(),
            Scenario::ResourceDiscoveryOversized => {
                resource.identity.native =
                    NativeIdentity::new("x".repeat(512)).qualified("native-property", "x".repeat(512));
            }
            _ => {}
        }
        Ok(if matches!(self.0.scenario, Scenario::ResourceDiscoveryDuplicate) {
            vec![resource.clone(), resource]
        } else {
            vec![resource]
        })
    }
    fn source_shutdown(&self) -> culprit_core::telemetry::SourceShutdown {
        if matches!(self.0.scenario, Scenario::ResourceDiscoveryUnconfirmed) {
            culprit_core::telemetry::SourceShutdown::Unconfirmed
        } else {
            culprit_core::telemetry::SourceShutdown::Quiescent
        }
    }
    fn engine(&self) -> Engine {
        Engine::Postgres
    }
    fn telemetry_source(&self, _: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        if matches!(
            self.0.scenario,
            Scenario::PagedOperational
                | Scenario::PagedWrongCounts
                | Scenario::PagedMissingClosure
                | Scenario::PagedBlockedClosure
        ) {
            return Ok(Box::new(PagedStateSource { state: self.0.clone(), offset: 0 }));
        }
        Ok(Box::new(TestSource(self.0.clone(), self.0.polls.fetch_add(1, Ordering::SeqCst))))
    }
    async fn probe(&self) -> Result<ServerInfo> {
        unreachable!("telemetry must not trigger a report probe")
    }
    async fn snapshot(&self, _: &str, _: &SnapshotOptions) -> Result<Snapshot> {
        unreachable!("telemetry must not trigger a scan")
    }
    async fn explain(&self, _: &str, _: ExplainMode) -> Result<Plan> {
        unreachable!()
    }
    async fn live(&self) -> Result<LiveState> {
        unreachable!()
    }
    async fn run(&self, _: &str, _: usize) -> Result<ResultSet> {
        unreachable!("telemetry uses its own bounded source contract")
    }
}
struct TestDirectory(tempfile::TempDir);
impl TestDirectory {
    fn new() -> Self {
        Self(tempfile::TempDir::with_prefix("culprit-collection-").unwrap())
    }
    fn output(&self) -> PathBuf {
        self.0.path().join("observation")
    }
    fn workspace(&self, state: Arc<State>) -> Workspace {
        let ws = Workspace::open_in(self.0.path().join("workspace")).with_provider(
            Engine::Postgres,
            culprit_engine::ProviderRegistration::new(
                Arc::new(move |_| {
                    state.factories.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::new(TestProvider(state.clone())))
                }),
                Vec::new(),
            )
            .unwrap(),
        );
        ws.add_connection(NewConnection {
            name: "pilot".into(),
            input: "postgres://reader@fixture.invalid/sample".into(),
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap();
        ws
    }
    fn manifest(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.output().join("manifest.json")).unwrap()).unwrap()
    }
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: culprit_core::telemetry::CollectionSelection::QueryHistory {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T07:00:00Z".into(),
        },
        budget: CollectionBudget::default(),
    }
}

fn resource_discovery_request() -> culprit_core::telemetry::ResourceDiscoveryRequest {
    culprit_core::telemetry::ResourceDiscoveryRequest {
        kind: culprit_core::telemetry::SharedResourceKind::Instance,
        budget: CollectionBudget::default(),
    }
}

#[tokio::test]
async fn resource_discovery_checks_budgets_before_authentication_and_releases_confirmed_ownership() {
    let directory = TestDirectory::new();
    let state = State::new(Scenario::Success);
    let workspace = directory.workspace(state.clone());
    let mut request = resource_discovery_request();
    request.budget.max_queries = 0;
    assert!(workspace.discover_resources("pilot", &request).await.is_err());
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    assert_eq!(state.calls.load(Ordering::SeqCst), 0);
    request.budget.max_queries = CollectionBudget::default().max_queries;
    let resources = workspace.discover_resources("pilot", &request).await.unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].identity.native.id, "native-instance");
    assert!(!workspace.collection_admission_status().await.unwrap().source_quiescence_required);
    workspace.discover_resources("pilot", &request).await.unwrap();
    assert_eq!(state.factories.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn resource_discovery_rejects_invalid_plugin_results_without_mistaking_them_for_native_shutdown_debt() {
    for scenario in [
        Scenario::ResourceDiscoveryDuplicate,
        Scenario::ResourceDiscoveryWrongKind,
        Scenario::ResourceDiscoveryWrongEngine,
        Scenario::ResourceDiscoveryMalformed,
        Scenario::ResourceDiscoveryOversized,
    ] {
        let directory = TestDirectory::new();
        let state = State::new(scenario);
        let workspace = directory.workspace(state.clone());
        let mut request = resource_discovery_request();
        request.budget.page_bytes = 1024;
        let error = workspace.discover_resources("pilot", &request).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
        assert!(!workspace.collection_admission_status().await.unwrap().source_quiescence_required);
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    }
    let directory = TestDirectory::new();
    let workspace = directory.workspace(State::new(Scenario::Empty));
    assert!(workspace.discover_resources("pilot", &resource_discovery_request()).await.unwrap().is_empty());
    let directory = TestDirectory::new();
    let workspace = directory.workspace(State::new(Scenario::ResourceDiscoveryDenied));
    assert_eq!(
        workspace.discover_resources("pilot", &resource_discovery_request()).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert!(!workspace.collection_admission_status().await.unwrap().source_quiescence_required);
}

#[tokio::test]
async fn resource_discovery_cannot_claim_success_after_unconfirmed_native_shutdown() {
    let directory = TestDirectory::new();
    let state = State::new(Scenario::ResourceDiscoveryUnconfirmed);
    let workspace = directory.workspace(state.clone());
    let request = resource_discovery_request();
    let error = workspace.discover_resources("pilot", &request).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert!(workspace.collection_admission_status().await.unwrap().source_quiescence_required);
    assert!(workspace.discover_resources("pilot", &request).await.is_err());
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropping_resource_discovery_preserves_admission_debt_and_blocks_a_replacement_provider() {
    let directory = TestDirectory::new();
    let state = State::new(Scenario::Blocked);
    let workspace = directory.workspace(state.clone());
    let request = resource_discovery_request();
    {
        let discovery = workspace.discover_resources("pilot", &request);
        tokio::pin!(discovery);
        tokio::select! {
            _ = &mut discovery => panic!("fixture discovery must wait"),
            result = tokio::time::timeout(std::time::Duration::from_secs(2), state.entered.notified()) => result.unwrap(),
        }
    }
    assert!(!state.active.load(Ordering::SeqCst));
    assert!(workspace.collection_admission_status().await.unwrap().source_quiescence_required);
    assert!(workspace.discover_resources("pilot", &request).await.is_err());
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fleet_capture_uses_the_same_outbox_and_native_shutdown_fence_as_one_off_collection() {
    use culprit_collector::{
        capture::CollectionAdmissionWindow,
        fleet::{FleetBackend, FleetSourceAdmission},
        outbox::Outbox,
    };
    for scenario in [Scenario::Success, Scenario::Unconfirmed] {
        let dir = TestDirectory::new();
        let state = State::new(scenario);
        let ws = dir.workspace(state.clone());
        let mut outbox = Outbox::create(&dir.0.path().join("outbox"), Default::default()).unwrap();
        outbox.recover().await.unwrap();
        let collection = request();
        let admission = FleetSourceAdmission {
            window: CollectionAdmissionWindow::new(tokio::time::Instant::now(), &collection.budget).unwrap(),
            route_revision: FleetBackend::route_revision(&ws, "pilot").unwrap(),
            investigation: None,
        };
        let outcome =
            FleetBackend::capture(&ws, "pilot", collection, &mut outbox, &CollectionCancellation::default(), admission)
                .await;
        assert_eq!(
            outcome.source_work,
            if matches!(scenario, Scenario::Unconfirmed) {
                SourceWorkState::Unconfirmed
            } else {
                SourceWorkState::Quiescent
            }
        );
        let manifest = outcome.result.unwrap();
        assert!(manifest.capture.revision.is_some());
        if matches!(scenario, Scenario::Unconfirmed) {
            assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed));
            let factories = state.factories.load(Ordering::SeqCst);
            assert!(
                ws.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.is_err()
            );
            assert_eq!(state.factories.load(Ordering::SeqCst), factories);
        } else {
            assert_eq!(manifest.status, CollectionStatus::Complete);
            assert_eq!(outbox.status(1, None).await.unwrap().ready, 1);
            assert!(state.closed.load(Ordering::SeqCst));
        }
    }
}

#[tokio::test]
async fn fleet_refuses_implicit_cli_sign_in_before_resolving_a_provider() {
    use culprit_collector::fleet::FleetBackend;
    let dir = TestDirectory::new();
    let state = State::new(Scenario::Success);
    let ws = dir.workspace(state.clone());
    ws.add_connection(NewConnection {
        name: "azure".into(),
        input: "mssql://fixture.database.windows.net/sample".into(),
        auth: Some(AuthKind::Entra),
        ..Default::default()
    })
    .unwrap();
    assert!(FleetBackend::validate_target(&ws, "azure").is_err());
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    let ws = ws.with_database_tokens(Arc::new(
        culprit_auth_azure::AzureCredential::new(culprit_auth_azure::IdentityConfig::ManagedIdentity {
            client_id: None,
        })
        .unwrap(),
    ));
    FleetBackend::validate_target(&ws, "azure").unwrap();
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn complete_artifacts_preserve_exact_counters_dimensions_and_empty_streams() {
    use sha2::{Digest, Sha256};
    let dir = TestDirectory::new();
    let state = State::new(Scenario::Success);
    let ws = dir.workspace(state.clone());
    let result = ws.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.unwrap();
    assert_eq!(result.status, CollectionStatus::Complete);
    assert!(state.closed.load(Ordering::SeqCst));
    assert_eq!(result.files[0].rows, 2);
    assert_eq!(result.files[1].rows, 0);
    for file in &result.files {
        let text = std::fs::read_to_string(dir.output().join(&file.file)).unwrap();
        for line in text.lines() {
            let row: Value = serde_json::from_str(line).unwrap();
            assert_eq!(row["record"]["stream"], serde_json::to_value(file.stream).unwrap());
        }
    }
    let bytes = std::fs::read(dir.output().join("runtime.ndjson")).unwrap();
    let rows = String::from_utf8(bytes.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows[0]["record"]["data"]["executions"], "9007199254740993");
    assert_eq!(rows[0]["record"]["data"]["key"]["native"]["qualifiers"]["plan_id"], "9007199254740993");
    assert_eq!(rows[0]["record"]["data"]["key"]["native"]["qualifiers"]["execution_type"], "3");
    let hash = Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    assert_eq!(result.files[0].sha256.as_deref(), Some(hash.as_str()));
    assert_eq!(std::fs::metadata(dir.output().join("waits.ndjson")).unwrap().len(), 0);
    assert!(!dir.output().join("runtime.ndjson.part").exists());
    assert!(result.references.complete);
    assert_eq!(result.references.plans.required, 1);
    for file in ["queries.ndjson", "plans.ndjson"] {
        let text = std::fs::read_to_string(dir.output().join(file)).unwrap();
        assert!(!text.contains("secret-value") && !text.contains("secret-comment"));
    }
}

#[tokio::test]
async fn late_failure_keeps_the_partial_stream_incomplete_without_hiding_the_other_stream() {
    let dir = TestDirectory::new();
    let state = State::new(Scenario::LateFailure);
    let ws = dir.workspace(state.clone());
    assert!(ws.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.is_err());
    let manifest = dir.manifest();
    assert_eq!(manifest["status"]["state"], "incomplete");
    assert_eq!(manifest["files"][0]["status"]["state"], "incomplete");
    assert_eq!(manifest["files"][1]["status"]["state"], "complete");
    assert!(dir.output().join("runtime.ndjson.part").exists());
    assert!(!dir.output().join("runtime.ndjson").exists());
    assert!(state.closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn cancellation_and_deadlines_close_owned_work_before_returning() {
    for timeout in [false, true] {
        let dir = TestDirectory::new();
        let state = State::new(Scenario::Blocked);
        let ws = dir.workspace(state.clone());
        let cancellation = CollectionCancellation::default();
        let stop = cancellation.clone();
        let output = dir.output();
        let mut req = request();
        if timeout {
            req.budget.statement_timeout_ms = 10;
            // Allow durable admission to complete before exercising a deadline during native work.
            req.budget.total_timeout_ms = 1000;
        }
        let task = tokio::spawn(async move { ws.collect_once("pilot", req, &output, &stop).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), state.entered.notified()).await.unwrap();
        assert_eq!(dir.manifest()["status"]["state"], "incomplete");
        if !timeout {
            cancellation.cancel();
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), task).await.unwrap().unwrap().unwrap();
        assert_eq!(
            result.status,
            CollectionStatus::Incomplete(if timeout { CollectionStop::TimedOut } else { CollectionStop::Cancelled })
        );
        assert!(state.closed.load(Ordering::SeqCst));
        assert!(!state.active.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn malformed_pages_cycles_and_duplicates_cannot_complete_an_observation() {
    for scenario in [Scenario::Cycle, Scenario::Duplicate, Scenario::NonFinite] {
        let dir = TestDirectory::new();
        let ws = dir.workspace(State::new(scenario));
        let result =
            ws.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.unwrap();
        assert_eq!(result.status, CollectionStatus::Incomplete(CollectionStop::InvalidPage));
    }
}

#[tokio::test]
async fn byte_and_page_limits_leave_explicit_partial_coverage() {
    for byte_limit in [false, true] {
        let dir = TestDirectory::new();
        let ws = dir.workspace(State::new(if byte_limit { Scenario::LargePage } else { Scenario::Success }));
        let mut req = request();
        if byte_limit {
            req.budget.page_bytes = 1024;
        } else {
            req.budget.max_pages = 1;
        }
        let result = ws.collect_once("pilot", req, &dir.output(), &CollectionCancellation::default()).await.unwrap();
        assert_eq!(
            result.status,
            CollectionStatus::Incomplete(if byte_limit {
                CollectionStop::ByteLimit
            } else {
                CollectionStop::PageLimit
            })
        );
        if byte_limit {
            assert_eq!(std::fs::metadata(dir.output().join("runtime.ndjson.part")).unwrap().len(), 0);
        } else {
            assert_eq!(result.files.iter().map(|file| file.pages).sum::<u32>(), 1);
        }
    }
}

#[tokio::test]
async fn source_identity_change_invalidates_previously_sealed_streams() {
    let dir = TestDirectory::new();
    let ws = dir.workspace(State::new(Scenario::ChangedIdentity));
    let result = ws.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.unwrap();
    assert!(result.files.iter().all(|file| file.status == CollectionStatus::Incomplete(CollectionStop::SourceChanged)));
}

#[tokio::test]
async fn existing_output_and_invalid_requests_never_reach_a_source() {
    let dir = TestDirectory::new();
    let state = State::new(Scenario::Empty);
    let ws = dir.workspace(state.clone());
    std::fs::create_dir(dir.output()).unwrap();
    std::fs::write(dir.output().join("owned-by-user"), b"untouched").unwrap();
    assert!(ws.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.is_err());
    assert_eq!(std::fs::read(dir.output().join("owned-by-user")).unwrap(), b"untouched");
    let mut req = request();
    req.budget.max_pages = 0;
    assert!(
        ws.collect_once("pilot", req, &dir.0.path().join("invalid"), &CollectionCancellation::default()).await.is_err()
    );
    assert!(!dir.0.path().join("invalid").exists());
    assert_eq!(state.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unconfirmed_cancellation_blocks_further_collection_in_the_workspace() {
    for scenario in [Scenario::Unconfirmed, Scenario::MetricsStillRunning] {
        let dir = TestDirectory::new();
        let state = State::new(scenario);
        let ws = dir.workspace(state.clone());
        let result =
            ws.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.unwrap();
        assert_eq!(result.status, CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed));
        let reads = state.calls.load(Ordering::SeqCst);
        let error = ws
            .collect_once("pilot", request(), &dir.0.path().join("blocked"), &CollectionCancellation::default())
            .await
            .unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
        assert_eq!(state.calls.load(Ordering::SeqCst), reads);
    }
}
#[tokio::test]
async fn standalone_unconfirmed_work_blocks_native_reads_after_workspace_restart() {
    let dir = TestDirectory::new();
    let workspace = dir.workspace(State::new(Scenario::Unconfirmed));
    let result =
        workspace.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.unwrap();
    assert_eq!(result.status, CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed));
    drop(workspace);
    let state = State::new(Scenario::Success);
    let provider_state = state.clone();
    let restarted = Workspace::open_in(dir.0.path().join("workspace")).with_provider(
        Engine::Postgres,
        culprit_engine::ProviderRegistration::new(
            Arc::new(move |_| {
                provider_state.factories.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(TestProvider(provider_state.clone())))
            }),
            Vec::new(),
        )
        .unwrap(),
    );
    let attempt = restarted
        .collect_once("pilot", request(), &dir.0.path().join("after-restart"), &CollectionCancellation::default())
        .await;
    assert!(attempt.is_err(), "uncertainty must survive a new Workspace instance");
    assert_eq!(state.factories.load(Ordering::SeqCst), 0, "block before provider/authentication creation");
    assert_eq!(state.calls.load(Ordering::SeqCst), 0, "block before native source work");
    let pending = restarted.collection_admission_status().await.unwrap();
    assert_eq!(pending.observation_id.as_deref(), Some(result.capture.observation_id.as_str()));
    restarted.acknowledge_collection_quiescence(&result.capture.observation_id).await.unwrap();
    let resumed = restarted
        .collect_once("pilot", request(), &dir.0.path().join("resumed"), &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(resumed.status, CollectionStatus::Complete);
    assert!(!restarted.collection_admission_status().await.unwrap().source_quiescence_required);
}

#[tokio::test]
async fn malformed_optional_read_metrics_cannot_publish_a_complete_capture() {
    let dir = TestDirectory::new();
    let state = State::new(Scenario::MalformedMetrics);
    let workspace = dir.workspace(state);
    let manifest =
        workspace.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::InvalidPage));
    assert!(manifest.read_metrics.is_none());
}

#[tokio::test]
async fn dropping_an_inflight_caller_retains_admission_debt() {
    let dir = TestDirectory::new();
    let state = State::new(Scenario::Blocked);
    let ws = Arc::new(dir.workspace(state.clone()));
    let output = dir.output();
    let collecting = ws.clone();
    let task = tokio::spawn(async move {
        collecting.collect_once("pilot", request(), &output, &CollectionCancellation::default()).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), state.entered.notified()).await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let error = ws
        .collect_once("pilot", request(), &dir.0.path().join("blocked"), &CollectionCancellation::default())
        .await
        .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(dir.manifest()["status"]["state"], "incomplete");
}

#[tokio::test]
async fn repeated_native_row_ids_do_not_merge_distinct_replica_or_outcome_measurements() {
    let dir = TestDirectory::new();
    let ws = dir.workspace(State::new(Scenario::Dimensions));
    let result = ws.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.unwrap();
    assert_eq!(result.status, CollectionStatus::Complete);
    assert_eq!(result.files[0].rows, 3);
}

#[tokio::test]
async fn sealed_metrics_without_resolved_definitions_cannot_claim_complete_coverage() {
    for scenario in [Scenario::MissingDefinitions, Scenario::UnresolvedIdentity] {
        let dir = TestDirectory::new();
        let ws = dir.workspace(State::new(scenario));
        let result =
            ws.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.unwrap();
        assert_eq!(result.files[0].status, CollectionStatus::Complete);
        assert_eq!(result.status, CollectionStatus::Incomplete(CollectionStop::MissingDefinitions));
        assert_eq!(result.references.plans.unresolved, 1);
        assert!(!result.references.complete);
        let text = std::fs::read_to_string(dir.output().join("runtime.ndjson")).unwrap();
        let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["record_id"].is_null(), matches!(scenario, Scenario::UnresolvedIdentity));
    }
}

#[tokio::test]
async fn repeated_capture_preserves_record_ids_without_reusing_observation_ids() {
    let mut captures = vec![];
    for _ in 0..2 {
        let dir = TestDirectory::new();
        let ws = dir.workspace(State::new(Scenario::Success));
        let result =
            ws.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.unwrap();
        let lines = std::fs::read_to_string(dir.output().join("runtime.ndjson")).unwrap();
        let rows: Vec<Value> = lines.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        captures.push((result.capture.observation_id, rows));
    }
    assert_ne!(captures[0].0, captures[1].0);
    for (a, b) in captures[0].1.iter().zip(&captures[1].1) {
        assert_eq!(a["record_id"], b["record_id"]);
        assert!(a["record_id"].is_string());
    }
}

#[tokio::test]
async fn the_export_boundary_redacts_payloads_supplied_by_an_external_provider() {
    let dir = TestDirectory::new();
    let ws = dir.workspace(State::new(Scenario::UnredactedDefinitions));
    let result = ws.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.unwrap();
    assert_eq!(result.status, CollectionStatus::Complete);
    for name in ["queries.ndjson", "plans.ndjson"] {
        let text = std::fs::read_to_string(dir.output().join(name)).unwrap();
        assert!(!text.contains("secret-value") && !text.contains("secret-comment"));
        let row: culprit_core::telemetry::ExportedRecord = serde_json::from_str(text.trim()).unwrap();
        assert!(row.record.is_valid());
    }
}

#[tokio::test]
async fn pressure_during_source_verification_withholds_completion_and_closes_owned_work() {
    let dir = TestDirectory::new();
    let state = State::new(Scenario::PressureAtVerification);
    let ws = dir.workspace(state.clone());
    let result = ws.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.unwrap();
    assert_eq!(result.status, CollectionStatus::Incomplete(CollectionStop::Pressure));
    assert!(result.files.iter().all(|file| file.status == CollectionStatus::Incomplete(CollectionStop::Pressure)));
    assert!(!result.references.complete);
    assert!(state.closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn journaled_collection_commits_local_bytes_before_exposing_a_ready_capture() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::{OutboxCaptureState, OutboxLimits};
    let dir = TestDirectory::new();
    let state = State::new(Scenario::Success);
    let ws = dir.workspace(state.clone());
    let root = dir.0.path().join("outbox");
    let mut outbox = Outbox::create(&root, OutboxLimits::default()).unwrap();
    let result =
        ws.collect_into_outbox("pilot", request(), &mut outbox, &CollectionCancellation::default()).await.unwrap();
    assert!(state.closed.load(Ordering::SeqCst));
    assert!(result.capture.revision.is_some());
    let status = outbox.status(10, None).await.unwrap();
    assert_eq!(status.ready, 1);
    assert_eq!(status.entries[0].state, OutboxCaptureState::Ready);
    let calls = state.calls.load(Ordering::SeqCst);
    let replay = Outbox::replay(&root, &result.capture.observation_id, &dir.0.path().join("replay")).await.unwrap();
    assert_eq!(replay.capture, result.capture);
    assert_eq!(state.calls.load(Ordering::SeqCst), calls);
}

#[tokio::test]
async fn outbox_capacity_blocks_before_a_provider_or_native_read_is_created() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::OutboxLimits;
    let dir = TestDirectory::new();
    let state = State::new(Scenario::Success);
    let ws = dir.workspace(state.clone());
    let mut outbox =
        Outbox::create(&dir.0.path().join("outbox"), OutboxLimits { max_bytes: 1024 * 1024, max_captures: 1 }).unwrap();
    assert!(ws.collect_into_outbox("pilot", request(), &mut outbox, &CollectionCancellation::default()).await.is_err());
    assert_eq!(state.calls.load(Ordering::SeqCst), 0);
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    assert_eq!(outbox.status(10, None).await.unwrap().captures, 0);
}

#[tokio::test]
async fn unconfirmed_native_work_remains_blocked_after_outbox_and_workspace_restart() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::OutboxLimits;
    let dir = TestDirectory::new();
    let state = State::new(Scenario::Unconfirmed);
    let ws = dir.workspace(state.clone());
    let root = dir.0.path().join("outbox");
    let mut outbox = Outbox::create(&root, OutboxLimits::default()).unwrap();
    let result =
        ws.collect_into_outbox("pilot", request(), &mut outbox, &CollectionCancellation::default()).await.unwrap();
    assert_eq!(result.status, CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed));
    drop(outbox);
    let mut outbox = Outbox::open(&root).unwrap();
    let new_state = State::new(Scenario::Success);
    let provider_state = new_state.clone();
    let restarted = Workspace::open_in(dir.0.path().join("workspace")).with_provider(
        Engine::Postgres,
        culprit_engine::ProviderRegistration::new(
            Arc::new(move |_| {
                provider_state.factories.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(TestProvider(provider_state.clone())))
            }),
            Vec::new(),
        )
        .unwrap(),
    );
    let error = restarted
        .collect_into_outbox("pilot", request(), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(new_state.calls.load(Ordering::SeqCst), 0);
    assert_eq!(new_state.factories.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn dropping_journaled_native_work_requires_quiescence_after_restart() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::OutboxLimits;
    let dir = TestDirectory::new();
    let state = State::new(Scenario::Blocked);
    let ws = dir.workspace(state.clone());
    let root = dir.0.path().join("outbox");
    let mut outbox = Outbox::create(&root, OutboxLimits::default()).unwrap();
    let task = tokio::spawn(async move {
        ws.collect_into_outbox("pilot", request(), &mut outbox, &CollectionCancellation::default()).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), state.entered.notified()).await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let mut outbox = Outbox::open(&root).unwrap();
    let report = outbox.recover().await.unwrap();
    assert_eq!(report.source_debts, 1);
    assert_eq!(report.interrupted, 1);
    assert_eq!(outbox.status(10, None).await.unwrap().ready, 0);
}

fn resource_record(at: &str) -> TelemetryRecord {
    use culprit_core::telemetry::*;
    TelemetryRecord::ResourceSample(ResourceMeasurement {
        at: at.into(),
        replica_id: Some("primary".into()),
        metrics: vec![ResourceMetricSample {
            key: "cpu".into(),
            scope: culprit_core::MeasurementScope::Database,
            unit: ResourceUnit::Percent,
            statistic: ResourceStatistic::Mean,
            period_ms: Some(15_000),
            limit_basis: ResourceLimitBasis::DatabaseLimit,
            value: Some(0.0),
        }],
    })
}
fn resource_description(scenario: Scenario) -> CollectionDescription {
    use culprit_core::telemetry::*;
    CollectionDescription {
        target: description().target,
        source: "fixture-resource-statistics".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::RetainedSamples(SampleHistoryDescription {
            from: request().bounds().unwrap().0.to_rfc3339(),
            until: request().bounds().unwrap().1.to_rfc3339(),
            observed_at: "2020-01-01T08:00:00Z".into(),
            available_from: Some(
                if matches!(scenario, Scenario::ExpiredResourceHistory) {
                    "2020-01-01T06:00:15Z"
                } else {
                    "2020-01-01T05:50:00Z"
                }
                .into(),
            ),
            available_until: Some("2020-01-01T07:05:00Z".into()),
            expected_samples: if matches!(scenario, Scenario::ResourceCountMismatch) { 2 } else { 1 },
            nominal_period_ms: Some(15_000),
        }),
    }
}
fn resource_request() -> CollectionRequest {
    CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: culprit_core::telemetry::CollectionSelection::ResourceHistory {
            from: request().bounds().unwrap().0.to_rfc3339(),
            until: request().bounds().unwrap().1.to_rfc3339(),
        },
        ..request()
    }
}
#[tokio::test]
async fn a_runtime_provider_can_capture_and_replay_resource_history_without_query_store_files() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::OutboxLimits;
    let dir = TestDirectory::new();
    let state = State::new(Scenario::ResourceHistory);
    let ws = dir.workspace(state.clone());
    let root = dir.0.path().join("resources");
    let mut outbox = Outbox::create(&root, OutboxLimits::default()).unwrap();
    let manifest = ws
        .collect_into_outbox("pilot", resource_request(), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(manifest.files[0].file, "resources.ndjson");
    assert_eq!(manifest.files[0].rows, 1);
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    assert!(state.closed.load(Ordering::SeqCst));
    let calls = state.calls.load(Ordering::SeqCst);
    let output = dir.0.path().join("replayed");
    Outbox::replay(&root, &manifest.capture.observation_id, &output).await.unwrap();
    assert_eq!(state.calls.load(Ordering::SeqCst), calls);
    assert!(output.join("resources.ndjson").exists());
    assert!(!output.join("runtime.ndjson").exists());
    let row: Value =
        serde_json::from_str(std::fs::read_to_string(output.join("resources.ndjson")).unwrap().trim()).unwrap();
    assert_eq!(row["record"]["data"]["metrics"][0]["value"], 0.0);
    assert_eq!(outbox.status(10, None).await.unwrap().ready, 1);
}
#[tokio::test]
async fn expired_or_inconsistent_resource_history_never_advances_the_outbox_head() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::OutboxLimits;
    for (scenario, reason) in [
        (Scenario::ExpiredResourceHistory, CollectionStop::RetentionGap),
        (Scenario::ResourceCountMismatch, CollectionStop::SourceChanged),
    ] {
        let dir = TestDirectory::new();
        let state = State::new(scenario);
        let ws = dir.workspace(state);
        let root = dir.0.path().join("outbox");
        let mut outbox = Outbox::create(&root, OutboxLimits::default()).unwrap();
        let manifest = ws
            .collect_into_outbox("pilot", resource_request(), &mut outbox, &CollectionCancellation::default())
            .await
            .unwrap();
        assert_eq!(manifest.status, CollectionStatus::Incomplete(reason));
        assert_eq!(outbox.status(10, None).await.unwrap().ready, 0);
        assert_eq!(manifest.files[0].rows, 1);
    }
}
#[tokio::test]
async fn equivalent_resource_timestamps_cannot_create_duplicate_measurements() {
    let dir = TestDirectory::new();
    let state = State::new(Scenario::DuplicateResourceSample);
    let ws = dir.workspace(state);
    let output = dir.0.path().join("samples");
    let manifest =
        ws.collect_once("pilot", resource_request(), &output, &CollectionCancellation::default()).await.unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::InvalidPage));
    assert_eq!(manifest.files[0].rows, 0);
}

fn activity_row() -> culprit_core::telemetry::ActivityObservation {
    use culprit_core::telemetry::*;
    ActivityObservation {
        blocking_status: culprit_core::telemetry::ActivityBlockingStatus::Known,
        key: ActivityKey {
            session: NativeActivityIdentity { id: "42".into(), generation: "login-1".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "request-1".into() }),
            transaction: None,
        },
        idle: false,
        scope: ActivityScopeBasis::RequestDatabase,
        state: "running".into(),
        database: Some("sample".into()),
        command: Some("SELECT".into()),
        last_request_age_ms: None,
        open_transactions: Some(0),
        elapsed_ms: Some("100000".into()),
        transaction_age_ms: None,
        session_counters: ActivityCounters::default(),
        request_counters: Some(ActivityCounters { cpu_ms: Some("0".into()), ..ActivityCounters::default() }),
        wait: None,
        waiting_on_lock: None,
        query_hash: Some("query-hash".into()),
        statement_identity: None,
        client: None,
        statement: DefinitionPayload::Available {
            text: "SELECT N'activity-canary-secret'".into(),
            sha256: culprit_core::telemetry::digest(b"SELECT N'activity-canary-secret'"),
            redaction_version: 1,
        },
    }
}
fn activity_rows(scenario: Scenario, step: usize) -> Vec<culprit_core::telemetry::ActivityObservation> {
    use culprit_core::telemetry::*;
    if matches!(scenario, Scenario::ActivitySequence) && step >= 4
        || matches!(scenario, Scenario::ActivityRestricted) && step > 0
    {
        return vec![];
    }
    let mut row = activity_row();
    if matches!(scenario, Scenario::ActivityConflictingRows) {
        let first = row.clone();
        row.key.session.generation = "recycled-login".into();
        return vec![first, row];
    }
    if matches!(scenario, Scenario::ActivityLockPoint) {
        row.blocking_status = ActivityBlockingStatus::Unknown;
        row.state = "suspended".into();
        row.waiting_on_lock = Some(true);
        row.elapsed_ms = Some("40000".into());
        row.wait = Some(ActivityWait {
            native_type: Some("LCK_M_X".into()),
            elapsed_ms: None,
            identified_resources: vec![],
            owners: vec![],
            resources: None,
        });
    }
    vec![row]
}
fn activity_description(scenario: Scenario, step: usize) -> CollectionDescription {
    use culprit_core::telemetry::*;
    let count = activity_rows(scenario, step).len() as u32;
    CollectionDescription {
        target: description().target,
        source: "activity-fixture".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some("native-epoch-1".into()),
            observed_from: format!("2020-01-01T06:{step:02}:00Z"),
            observed_until: format!("2020-01-01T06:{step:02}:00Z"),
            at: format!("2020-01-01T06:{step:02}:00Z"),
            scope: culprit_core::MeasurementScope::Database,
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            native_rows: count,
            observations: count,
            coverage: StateSourceCoverage::observed(
                "activity-fixture".into(),
                count,
                !matches!(scenario, Scenario::ActivityRestricted),
            ),
        }),
    }
}
fn activity_request() -> CollectionRequest {
    use culprit_core::telemetry::*;
    CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: CollectionBudget::default(),
    }
}
async fn analyze_activity_checkpoint(
    outbox: &mut culprit_collector::outbox::Outbox,
) -> culprit_core::telemetry::DiagnosticCheckpoint {
    use culprit_collector::diagnostics::{DiagnosticPolicies, drain_pending};
    use culprit_core::telemetry::*;
    let policies =
        DiagnosticPolicies::for_activity("pilot", CollectionSubject::Database, ActivityPolicy::default()).unwrap();
    drain_pending(outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    assert!(outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    let mut page = outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 128).await.unwrap();
    assert!(page.next_cursor.is_none());
    assert_eq!(page.checkpoints.len(), 1);
    page.checkpoints.remove(0)
}
#[tokio::test]
async fn sealed_baselines_survive_reopen_and_failed_polls_without_resolving_or_restarting_the_episode() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::*;
    let dir = TestDirectory::new();
    let state = State::new(Scenario::ActivitySequence);
    let ws = dir.workspace(state.clone());
    let root = dir.0.path().join("activity-outbox");
    let mut outbox = Outbox::create(&root, OutboxLimits::default()).unwrap();
    let first = ws
        .collect_into_outbox("pilot", activity_request(), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(first.status, CollectionStatus::Complete);
    assert_eq!(
        first.files.iter().map(|file| file.stream).collect::<Vec<_>>(),
        vec![TelemetryStream::ActivityObservations]
    );
    assert!(
        outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 128).await.unwrap().checkpoints.is_empty()
    );
    assert_eq!(outbox.status(10, None).await.unwrap().ready, 1, "the baseline is durable before analysis");
    let first_checkpoint = analyze_activity_checkpoint(&mut outbox).await;
    assert_eq!(first_checkpoint.input.capture, first.capture);
    assert_eq!(first_checkpoint.episodes.len(), 1);
    let original = first_checkpoint.episodes[0].clone();
    assert_eq!(original.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(original.first.capture, first.capture);
    drop(outbox);
    let mut outbox = Outbox::open(&root).unwrap();
    let failed = ws
        .collect_into_outbox("pilot", activity_request(), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap();
    assert!(matches!(failed.status, CollectionStatus::Incomplete(_)));
    assert_eq!(outbox.status(10, None).await.unwrap().ready, 2, "raw evidence and diagnostics are separate captures");
    assert_eq!(analyze_activity_checkpoint(&mut outbox).await, first_checkpoint);
    let next = ws
        .collect_into_outbox("pilot", activity_request(), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(next.status, CollectionStatus::Complete);
    let checkpoint = analyze_activity_checkpoint(&mut outbox).await;
    assert_eq!(checkpoint.input.capture, next.capture);
    assert_eq!(checkpoint.episodes[0].id, original.id);
    assert_eq!(checkpoint.episodes[0].first, original.first);
    assert_eq!(checkpoint.episodes[0].latest.capture, next.capture);
    assert_eq!(checkpoint.episodes[0].last_seen_at, "2020-01-01T06:02:00Z");
    let unchanged = ws
        .collect_into_outbox("pilot", activity_request(), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap();
    let unchanged_checkpoint = analyze_activity_checkpoint(&mut outbox).await;
    assert_eq!(unchanged_checkpoint.episodes[0].id, original.id);
    assert_eq!(unchanged_checkpoint.episodes[0].first_seen_at, original.first_seen_at);
    assert_eq!(unchanged_checkpoint.episodes[0].latest.capture, unchanged.capture);
    let cleared = ws
        .collect_into_outbox("pilot", activity_request(), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(cleared.status, CollectionStatus::Complete);
    let cleared_checkpoint = analyze_activity_checkpoint(&mut outbox).await;
    let recovered = &cleared_checkpoint.episodes[0];
    assert_eq!(recovered.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(recovered.id, original.id);
    assert_eq!(recovered.first, original.first);
    assert_eq!(recovered.latest.capture, unchanged.capture, "absence does not invent a native observation");
    assert_eq!(recovered.recovery.as_ref().unwrap().capture().capture, cleared.capture);
    assert_eq!(
        recovered.analysis,
        DiagnosticAnalysisStatus::Pending,
        "recovery does not erase historical analysis debt"
    );
    let replay = dir.0.path().join("activity-replay");
    Outbox::replay(&root, &next.capture.observation_id, &replay).await.unwrap();
    for file in &next.files {
        let text = std::fs::read_to_string(replay.join(&file.file)).unwrap();
        assert!(!text.contains("activity-canary-secret"));
    }
}
#[tokio::test]
async fn conflicting_native_session_generations_cannot_seal_a_baseline_or_derive_an_episode() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::*;
    let dir = TestDirectory::new();
    let state = State::new(Scenario::ActivityConflictingRows);
    let ws = dir.workspace(state);
    let root = dir.0.path().join("outbox");
    let mut outbox = Outbox::create(&root, OutboxLimits::default()).unwrap();
    let manifest = ws
        .collect_into_outbox("pilot", activity_request(), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::InvalidPage));
    assert!(!manifest.references.complete);
    assert_eq!(outbox.status(10, None).await.unwrap().ready, 0);
    assert!(outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    assert!(
        outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 128).await.unwrap().checkpoints.is_empty()
    );
}
#[tokio::test(start_paused = true)]
async fn activity_monitor_keeps_a_minute_between_attempts_and_reports_failed_polls_as_unknown() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::*;
    let dir = TestDirectory::new();
    let state = State::new(Scenario::ActivitySequence);
    let ws = dir.workspace(state);
    let root = dir.0.path().join("outbox");
    let mut outbox = Outbox::create(&root, OutboxLimits::default()).unwrap();
    let options = ActivityMonitorOptions { max_polls: Some(3), ..ActivityMonitorOptions::default() };
    let mut reports = Vec::new();
    let summary = ws
        .monitor_activity("pilot", &options, &mut outbox, &CollectionCancellation::default(), |event| {
            reports.push((tokio::time::Instant::now(), event.clone()));
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!((summary.attempts, summary.complete, summary.unknown), (3, 2, 1));
    assert!(reports.windows(2).all(|pair| pair[1].0.duration_since(pair[0].0) >= std::time::Duration::from_secs(60)));
    assert_eq!((reports[0].1.active_episodes, reports[0].1.uncertain_episodes), (Some(1), Some(0)));
    assert!(reports[1].1.active_episodes.is_none() && reports[1].1.uncertain_episodes.is_none());
    assert_eq!((reports[2].1.active_episodes, reports[2].1.uncertain_episodes), (Some(1), Some(0)));
    assert!(matches!(reports[1].1.status, CollectionStatus::Incomplete(_)));
}

#[tokio::test(start_paused = true)]
async fn activity_monitor_stops_immediately_when_native_shutdown_is_unconfirmed() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::*;
    let dir = TestDirectory::new();
    let state = State::new(Scenario::ActivityUnconfirmed);
    let ws = dir.workspace(state.clone());
    let mut outbox = Outbox::create(&dir.0.path().join("outbox"), OutboxLimits::default()).unwrap();
    let mut reports = Vec::new();
    let started = tokio::time::Instant::now();
    let error = ws
        .monitor_activity(
            "pilot",
            &ActivityMonitorOptions::default(),
            &mut outbox,
            &CollectionCancellation::default(),
            |event| {
                reports.push(event.clone());
                Ok(())
            },
        )
        .await
        .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(reports.len(), 1);
    assert!(reports[0].active_episodes.is_none() && reports[0].uncertain_episodes.is_none());
    assert!(started.elapsed() < std::time::Duration::from_secs(60));
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn activity_monitor_cancels_while_idle_without_another_source_read() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::*;
    let dir = TestDirectory::new();
    let state = State::new(Scenario::ActivitySequence);
    let ws = dir.workspace(state.clone());
    let mut outbox = Outbox::create(&dir.0.path().join("outbox"), OutboxLimits::default()).unwrap();
    let cancellation = CollectionCancellation::default();
    let stopped = cancellation.clone();
    let summary = ws
        .monitor_activity("pilot", &ActivityMonitorOptions::default(), &mut outbox, &cancellation, move |_| {
            let stopped = stopped.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                stopped.cancel();
            });
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(summary.attempts, 1);
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    assert!(state.closed.load(Ordering::SeqCst));
}
#[tokio::test]
async fn restricted_activity_visibility_preserves_a_sealed_positive_baseline_and_qualified_coverage() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::*;
    let dir = TestDirectory::new();
    let ws = dir.workspace(State::new(Scenario::ActivityRestricted));
    let mut outbox = Outbox::create(&dir.0.path().join("outbox"), OutboxLimits::default()).unwrap();
    let manifest = ws
        .collect_into_outbox("pilot", activity_request(), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    assert!(manifest.references.complete);
    let native = manifest.description.as_ref().unwrap().observation.activity().unwrap();
    assert_eq!(native.coverage.state, culprit_core::SourceState::Partial);
    assert!(!native.coverage.complete_scope);
    assert_eq!(outbox.status(10, None).await.unwrap().ready, 1);
    let checkpoint = analyze_activity_checkpoint(&mut outbox).await;
    assert_eq!(checkpoint.episodes[0].lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(checkpoint.episodes[0].first.capture, manifest.capture);
}

#[tokio::test(start_paused = true)]
async fn activity_monitor_reports_partial_absence_as_uncertain_without_fabricating_recovery() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::*;
    let dir = TestDirectory::new();
    let ws = dir.workspace(State::new(Scenario::ActivityRestricted));
    let mut outbox = Outbox::create(&dir.0.path().join("outbox"), OutboxLimits::default()).unwrap();
    let options = ActivityMonitorOptions { max_polls: Some(2), ..Default::default() };
    let mut events = Vec::new();
    let summary = ws
        .monitor_activity("pilot", &options, &mut outbox, &CollectionCancellation::default(), |event| {
            events.push(event.clone());
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!((summary.attempts, summary.complete, summary.unknown), (2, 2, 0));
    assert_eq!((events[0].active_episodes, events[0].uncertain_episodes), (Some(1), Some(0)));
    assert_eq!((events[1].active_episodes, events[1].uncertain_episodes), (Some(0), Some(1)));
    assert!(events.iter().all(|event| event.native_coverage.as_ref().is_some_and(|coverage| !coverage.complete_scope)));
    let checkpoint = analyze_activity_checkpoint(&mut outbox).await;
    assert_eq!(checkpoint.episodes[0].lifecycle, DiagnosticLifecycle::Uncertain);
    assert!(checkpoint.episodes[0].recovery.is_none());
}

#[tokio::test]
async fn activity_monitor_retains_a_native_lock_point_without_turning_request_age_into_blocking_duration() {
    use culprit_collector::outbox::Outbox;
    use culprit_core::telemetry::*;
    let dir = TestDirectory::new();
    let ws = dir.workspace(State::new(Scenario::ActivityLockPoint));
    let mut outbox = Outbox::create(&dir.0.path().join("outbox"), OutboxLimits::default()).unwrap();
    let options = ActivityMonitorOptions { max_polls: Some(1), ..Default::default() };
    let mut events = Vec::new();
    let summary = ws
        .monitor_activity("pilot", &options, &mut outbox, &CollectionCancellation::default(), |event| {
            events.push(event.clone());
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!((summary.attempts, summary.complete, summary.unknown), (1, 1, 0));
    assert_eq!((events[0].active_episodes, events[0].uncertain_episodes), (Some(1), Some(0)));
    let checkpoint = analyze_activity_checkpoint(&mut outbox).await;
    assert_eq!(
        checkpoint.episodes.len(),
        1,
        "request age establishes neither aged blocking nor a long-running request"
    );
    let episode = &checkpoint.episodes[0];
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert!(matches!(&episode.signal, DiagnosticSignal::LockWait { key } if key == &activity_row().key));
    assert_eq!(episode.first.capture, events[0].capture.clone().unwrap());
    let (_, original) = outbox.diagnostic_evidence(&episode.first).await.unwrap();
    let TelemetryRecord::Activity(row) = original.record else {
        panic!("the incident must reference its original native activity")
    };
    assert_eq!(row.waiting_on_lock, Some(true));
    assert_eq!(row.elapsed_ms.as_deref(), Some("40000"));
    let wait = row.wait.as_ref().unwrap();
    assert_eq!(wait.native_type.as_deref(), Some("LCK_M_X"));
    assert!(wait.elapsed_ms.is_none() && wait.owners.is_empty() && wait.resources.is_none());
}

fn operational_description(scenario: Scenario) -> CollectionDescription {
    use culprit_core::telemetry::*;
    let mut description = description();
    let mismatch = matches!(scenario, Scenario::OperationalSourceCountMismatch);
    let mut sources = vec![StateSourceCoverage::collected("log".into(), 1)];
    if mismatch {
        sources.push(StateSourceCoverage::collected("files".into(), 1));
    }
    description.source = "operational-fixture".into();
    description.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::Operational,
        observed_from: "2020-01-01T06:00:00Z".into(),
        observed_until: "2020-01-01T06:00:01Z".into(),
        records: if mismatch { 2 } else { 1 },
        sources,
    });
    description
}
fn operational_rows(scenario: Scenario) -> Vec<TelemetryRecord> {
    use culprit_core::telemetry::*;
    if matches!(scenario, Scenario::CumulativeStatements) {
        let sample = StatementCounterSample {
            scope: culprit_core::MeasurementScope::Database,
            distributions: vec![],
            identity: NativeQueryIdentity {
                query_id: "opaque-query".into(),
                qualifiers: [("userid".into(), "17".into()), ("dbid".into(), "42".into())].into(),
            },
            observed_at: "2020-01-01T06:00:00Z".into(),
            reset: culprit_core::ResetMarker::Unknown,
            totals: vec![
                StatementCounter {
                    key: RuntimeMetricKey::Executions,
                    unit: RuntimeMetricUnit::Count,
                    total: MeasurementValue::Integer("9007199254740993".into()),
                },
                StatementCounter {
                    key: RuntimeMetricKey::Duration,
                    unit: RuntimeMetricUnit::Milliseconds,
                    total: MeasurementValue::Real(1.25),
                },
                StatementCounter {
                    key: RuntimeMetricKey::LogicalReads,
                    unit: RuntimeMetricUnit::Pages { bytes_per_page: 16384 },
                    total: MeasurementValue::Integer("7".into()),
                },
            ],
        };
        return vec![TelemetryRecord::Operational(OperationalObservation {
            source: "log".into(),
            key: sample.key().unwrap(),
            fact: OperationalFact::StatementCounters(Box::new(sample)),
        })];
    }
    let row = |key: &str| {
        TelemetryRecord::Operational(OperationalObservation {
            source: "log".into(),
            key: key.into(),
            fact: OperationalFact::Space(OperationalSpace {
                kind: culprit_core::FileKind::Log,
                scope: culprit_core::FileScope::Database,
                native_name: None,
                total_bytes: Some(1024.0),
                used_bytes: None,
                limit_bytes: None,
                growth_enabled: Some(true),
                reuse_wait: None,
                parts: vec![],
            }),
        })
    };
    if matches!(scenario, Scenario::OperationalSourceCountMismatch) { vec![row("a"), row("b")] } else { vec![row("a")] }
}
fn operational_request() -> CollectionRequest {
    CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: culprit_core::telemetry::CollectionSelection::Snapshot {
            dataset: culprit_core::telemetry::SnapshotDataset::Operational,
        },
        budget: CollectionBudget::default(),
    }
}
#[tokio::test]
async fn current_operational_facts_commit_and_replay_without_inventing_unknown_space_usage() {
    use culprit_collector::outbox::Outbox;
    let dir = TestDirectory::new();
    let workspace = dir.workspace(State::new(Scenario::OperationalState));
    let mut outbox = Outbox::create(&dir.0.path().join("outbox"), Default::default()).unwrap();
    let manifest = workspace
        .collect_into_outbox("pilot", operational_request(), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(manifest.files[0].file, "operations.ndjson");
    let replay =
        Outbox::replay(&dir.0.path().join("outbox"), &manifest.capture.observation_id, &dir.0.path().join("replay"))
            .await
            .unwrap();
    assert_eq!(replay.batches, 1);
    let content = std::fs::read_to_string(dir.0.path().join("replay/operations.ndjson")).unwrap();
    let value: Value = serde_json::from_str(content.trim()).unwrap();
    assert_eq!(value.pointer("/record/data/fact/data/used_bytes"), Some(&Value::Null));
}
#[tokio::test]
async fn matching_total_rows_cannot_hide_incorrect_per_source_operational_coverage() {
    let dir = TestDirectory::new();
    let workspace = dir.workspace(State::new(Scenario::OperationalSourceCountMismatch));
    let manifest = workspace
        .collect_once("pilot", operational_request(), &dir.output(), &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::InvalidPage));
    assert!(!manifest.references.complete);
}

#[tokio::test]
async fn paged_native_snapshots_require_final_coverage_and_replay_with_the_same_record_identities() {
    use culprit_collector::outbox::Outbox;
    for (scenario, expected) in [
        (Scenario::PagedOperational, CollectionStatus::Complete),
        (Scenario::PagedWrongCounts, CollectionStatus::Incomplete(CollectionStop::SourceChanged)),
        (Scenario::PagedMissingClosure, CollectionStatus::Incomplete(CollectionStop::InvalidPage)),
    ] {
        let dir = TestDirectory::new();
        let state = State::new(scenario);
        let workspace = dir.workspace(state.clone());
        let box_dir = dir.0.path().join("outbox");
        let mut outbox = Outbox::create(&box_dir, Default::default()).unwrap();
        let manifest = workspace
            .collect_into_outbox("pilot", operational_request(), &mut outbox, &CollectionCancellation::default())
            .await
            .unwrap();
        assert_eq!(manifest.status, expected);
        assert_eq!(manifest.files[0].rows, 2);
        assert_eq!(state.completions.load(Ordering::SeqCst), 1);
        assert!(state.closed.load(Ordering::SeqCst));
        if expected == CollectionStatus::Complete {
            assert!(matches!(
                manifest.description.as_ref().unwrap().observation,
                culprit_core::telemetry::SourceObservation::Snapshot(_)
            ));
            let replay = dir.0.path().join("replay");
            Outbox::replay(&box_dir, &manifest.capture.observation_id, &replay).await.unwrap();
            let original = box_dir.join("captures").join(&manifest.capture.observation_id);
            assert_eq!(
                std::fs::read(original.join("operations.ndjson")).unwrap(),
                std::fs::read(replay.join("operations.ndjson")).unwrap()
            );
        } else {
            assert_eq!(outbox.status(10, None).await.unwrap().ready, 0);
        }
    }
}

#[tokio::test]
async fn snapshot_row_budget_stops_paging_before_native_completion_or_publication() {
    let dir = TestDirectory::new();
    let state = State::new(Scenario::PagedOperational);
    let workspace = dir.workspace(state.clone());
    let mut request = operational_request();
    request.budget.max_snapshot_rows = 1;
    let manifest =
        workspace.collect_once("pilot", request, &dir.output(), &CollectionCancellation::default()).await.unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::RowLimit));
    assert_eq!(manifest.files[0].rows, 1);
    assert_eq!(state.completions.load(Ordering::SeqCst), 0);
    assert_eq!(state.calls.load(Ordering::SeqCst), 1, "no next native page is admitted after the row budget is full");
    assert!(state.closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn cancellation_during_native_completion_keeps_sealed_pages_unpublished_and_closes_the_owner() {
    let dir = TestDirectory::new();
    let state = State::new(Scenario::PagedBlockedClosure);
    let workspace = dir.workspace(state.clone());
    let cancellation = CollectionCancellation::default();
    let output = dir.output();
    let collect = workspace.collect_once("pilot", operational_request(), &output, &cancellation);
    let cancel = async {
        state.entered.notified().await;
        cancellation.cancel();
    };
    let (manifest, ()) = tokio::join!(collect, cancel);
    let manifest = manifest.unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::Cancelled));
    assert_eq!(manifest.files[0].rows, 2);
    assert!(!manifest.references.complete);
    assert!(state.closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn native_query_counters_without_plans_or_replica_dimensions_complete() {
    let dir = TestDirectory::new();
    let workspace = dir.workspace(State::new(Scenario::QueryOnly));
    let manifest =
        workspace.collect_once("pilot", request(), &dir.output(), &CollectionCancellation::default()).await.unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    assert!(manifest.references.complete);
    assert_eq!(manifest.references.queries.required, 1);
    assert_eq!(manifest.references.plans.required, 0);
    let rows = std::fs::read_to_string(dir.output().join("runtime.ndjson")).unwrap();
    let first: Value = serde_json::from_str(rows.lines().next().unwrap()).unwrap();
    assert!(first["record_id"].is_string());
    assert!(first["record"]["data"]["key"]["references"]["plan"].is_null());
    assert_eq!(first["record"]["data"]["metrics"].as_array().unwrap().len(), 1);
}
#[tokio::test]
async fn cumulative_statement_counters_survive_durable_outbox_restart_and_replay() {
    use culprit_collector::outbox::Outbox;
    let dir = TestDirectory::new();
    let workspace = dir.workspace(State::new(Scenario::CumulativeStatements));
    let path = dir.0.path().join("outbox");
    let mut outbox = Outbox::create(&path, Default::default()).unwrap();
    let manifest = workspace
        .collect_into_outbox("pilot", operational_request(), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    assert!(matches!(
        manifest.description.as_ref().unwrap().observation.range(),
        Ok(culprit_core::telemetry::TelemetryRange::Current)
    ));
    drop(outbox);
    let reopened = Outbox::open(&path).unwrap();
    assert_eq!(reopened.status(10, None).await.unwrap().ready, 1);
    Outbox::replay(&path, &manifest.capture.observation_id, &dir.0.path().join("replay")).await.unwrap();
    let rows = std::fs::read_to_string(dir.0.path().join("replay/operations.ndjson")).unwrap();
    let record: culprit_core::telemetry::ExportedRecord = serde_json::from_str(rows.trim()).unwrap();
    let culprit_core::telemetry::TelemetryRecord::Operational(row) = record.record else { panic!("operational") };
    let culprit_core::telemetry::OperationalFact::StatementCounters(sample) = row.fact else {
        panic!("statement counters")
    };
    assert_eq!(sample.reset, culprit_core::ResetMarker::Unknown);
    assert_eq!(sample.totals[0].total, MeasurementValue::Integer("9007199254740993".into()));
    assert_eq!(sample.totals.len(), 3, "unknown CPU measurements stay absent");
}

#[tokio::test]
async fn fleet_owner_receipt_after_restart_never_calls_an_older_durable_native_debt_quiet() {
    use culprit_collector::{
        capture::CollectionAdmissionWindow,
        fleet::{FleetBackend, FleetSourceAdmission},
        outbox::Outbox,
    };
    use culprit_core::telemetry::SourceWorkState;
    let dir = TestDirectory::new();
    let state = State::new(Scenario::Unconfirmed);
    let workspace = dir.workspace(state.clone());
    let outbox_path = dir.0.path().join("outbox");
    let mut outbox = Outbox::create(&outbox_path, Default::default()).unwrap();
    outbox.recover().await.unwrap();
    let collection = request();
    let admitted = FleetSourceAdmission {
        window: CollectionAdmissionWindow::new(tokio::time::Instant::now(), &collection.budget).unwrap(),
        route_revision: FleetBackend::route_revision(&workspace, "pilot").unwrap(),
        investigation: None,
    };
    let original = FleetBackend::capture(
        &workspace,
        "pilot",
        collection,
        &mut outbox,
        &CollectionCancellation::default(),
        admitted,
    )
    .await;
    assert_eq!(original.source_work, SourceWorkState::Unconfirmed);
    let original_attempt = original.attempt.unwrap();
    assert!(original_attempt.is_valid());
    assert!(original_attempt.manifest_sha256.is_some());
    let original = original.result.unwrap();
    assert_eq!(original_attempt.capture, original.capture);
    assert_eq!(original.status, CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed));
    let previous_debt = workspace.collection_admission_status().await.unwrap();
    assert!(previous_debt.source_quiescence_required);
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    drop(workspace);
    drop(outbox);

    let fresh = State::new(Scenario::Success);
    let provider = fresh.clone();
    let restarted = Workspace::open_in(dir.0.path().join("workspace")).with_provider(
        Engine::Postgres,
        culprit_engine::ProviderRegistration::new(
            Arc::new(move |_| {
                provider.factories.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(TestProvider(provider.clone())))
            }),
            Vec::new(),
        )
        .unwrap(),
    );
    let mut outbox = Outbox::open(&outbox_path).unwrap();
    let collection = request();
    let admitted = FleetSourceAdmission {
        window: CollectionAdmissionWindow::new(tokio::time::Instant::now(), &collection.budget).unwrap(),
        route_revision: FleetBackend::route_revision(&restarted, "pilot").unwrap(),
        investigation: None,
    };
    let outcome = FleetBackend::capture(
        &restarted,
        "pilot",
        collection,
        &mut outbox,
        &CollectionCancellation::default(),
        admitted,
    )
    .await;
    assert_eq!(
        outcome.source_work,
        SourceWorkState::NotStarted,
        "a new gate is no closure proof for an older owner's durable debt"
    );
    assert!(outcome.attempt.is_none(), "an old source debt must reject the new operation before allocation");
    assert_eq!(CollectionStop::from_error(&outcome.result.unwrap_err()), CollectionStop::CancellationUnconfirmed);
    assert_eq!(fresh.factories.load(Ordering::SeqCst), 0);
    assert_eq!(fresh.polls.load(Ordering::SeqCst), 0);
    assert_eq!(fresh.calls.load(Ordering::SeqCst), 0);
    assert!(!fresh.closed.load(Ordering::SeqCst));
    let remaining_debt = restarted.collection_admission_status().await.unwrap();
    assert!(remaining_debt.source_quiescence_required);
    assert_eq!(remaining_debt.observation_id, previous_debt.observation_id);
    assert_eq!(remaining_debt.connection, previous_debt.connection);
    let status = outbox.status(10, None).await.unwrap();
    assert_eq!((status.captures, status.source_debts, status.ready), (1, 1, 0));
    assert_eq!(status.entries[0].capture, original.capture);
    assert_eq!(status.entries[0].source_work, SourceWorkState::Unconfirmed);
}

#[tokio::test]
async fn fleet_owner_receipt_retains_confirmed_close_and_partial_native_evidence_when_capture_fails() {
    use culprit_collector::{
        capture::CollectionAdmissionWindow,
        fleet::{FleetBackend, FleetSourceAdmission},
        outbox::Outbox,
    };
    use culprit_core::telemetry::{CollectionManifest, ExportedRecord, OutboxCaptureState, SourceWorkState};
    let dir = TestDirectory::new();
    let state = State::new(Scenario::LateFailure);
    let workspace = dir.workspace(state.clone());
    let outbox_path = dir.0.path().join("outbox");
    let mut outbox = Outbox::create(&outbox_path, Default::default()).unwrap();
    outbox.recover().await.unwrap();
    let collection = request();
    let admitted = FleetSourceAdmission {
        window: CollectionAdmissionWindow::new(tokio::time::Instant::now(), &collection.budget).unwrap(),
        route_revision: FleetBackend::route_revision(&workspace, "pilot").unwrap(),
        investigation: None,
    };
    let outcome = FleetBackend::capture(
        &workspace,
        "pilot",
        collection,
        &mut outbox,
        &CollectionCancellation::default(),
        admitted,
    )
    .await;
    assert_eq!(outcome.source_work, SourceWorkState::Quiescent, "native closure is independent of the capture result");
    let receipt = outcome.attempt.unwrap();
    assert!(receipt.is_valid());
    assert_eq!(outcome.result.unwrap_err().code, ErrorCode::QueryFailed);
    assert!(state.closed.load(Ordering::SeqCst));
    assert!(!state.active.load(Ordering::SeqCst));
    assert!(!workspace.collection_admission_status().await.unwrap().source_quiescence_required);
    let status = outbox.status(10, None).await.unwrap();
    assert_eq!((status.captures, status.source_debts, status.ready), (1, 0, 0));
    let entry = &status.entries[0];
    assert_eq!(receipt.capture, entry.capture);
    assert_eq!(entry.source_work, SourceWorkState::Quiescent);
    assert_eq!(entry.state, OutboxCaptureState::Incomplete);
    let capture_directory = outbox_path.join("captures").join(&entry.capture.observation_id);
    let bytes = std::fs::read(capture_directory.join("manifest.json")).unwrap();
    assert_eq!(receipt.manifest_sha256, Some(culprit_core::telemetry::digest(&bytes)));
    let manifest: CollectionManifest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(manifest.capture, entry.capture);
    assert!(matches!(manifest.status, CollectionStatus::Incomplete(_)));
    assert!(!manifest.references.complete, "a retained prefix must not become complete workload evidence");
    let runtime = manifest.files.iter().find(|file| file.stream == TelemetryStream::QueryRuntime).unwrap();
    assert!(matches!(runtime.status, CollectionStatus::Incomplete(_)));
    assert_eq!(runtime.rows, 1);
    assert_eq!(runtime.file, "runtime.ndjson.part");
    let raw = std::fs::read_to_string(capture_directory.join(&runtime.file)).unwrap();
    let rows = raw.lines().map(|line| serde_json::from_str::<ExportedRecord>(line).unwrap()).collect::<Vec<_>>();
    let expected = ExportedRecord::new(manifest.description.as_ref().unwrap(), &entry.capture, measurement(1)).unwrap();
    assert_eq!(rows, vec![expected], "the established first native measurement survives the later failure exactly");
    let definitions = manifest
        .files
        .iter()
        .filter(|file| matches!(file.stream, TelemetryStream::QueryDefinitions | TelemetryStream::PlanDefinitions))
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 2);
    assert!(definitions.iter().all(|file| file.status == CollectionStatus::Complete));
    assert!(state.calls.load(Ordering::SeqCst) >= 2, "the test must actually fail after positive native evidence");
}
