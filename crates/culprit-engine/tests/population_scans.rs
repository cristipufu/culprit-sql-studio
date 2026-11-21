//! Batch scans acquire native inputs once, then analyze and cache exact populations after dropping that owner.
#[cfg(feature = "collector")]
mod completed_runtime;
use async_trait::async_trait;
use culprit_core::{
    AuthKind, Category, Check, CheckEvidence, CheckInfo, DatabaseIdentity, Engine, Error, ExplainMode, Finding,
    LiveState, Plan, PopulationSnapshots, Provider, Result, RuntimePopulation, RuntimePopulationCatalog, ServerInfo,
    Snapshot, SnapshotOptions,
    i18n::Message,
    telemetry::{CollectionStop, NativeIdentity, SourceShutdown},
};
use culprit_engine::{NewConnection, ProviderRegistration, Workspace};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct NativeState {
    snapshots: PopulationSnapshots,
    active: AtomicUsize,
    acquisitions: AtomicUsize,
    factories: AtomicUsize,
    deadlocks: AtomicUsize,
    events: AtomicUsize,
    analyzed: Mutex<Vec<Option<NativeIdentity>>>,
    shutdown: SourceShutdown,
    block_snapshots: AtomicBool,
    snapshot_entered: tokio::sync::Notify,
}

struct NativeProvider(Arc<NativeState>);
impl Drop for NativeProvider {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl Provider for NativeProvider {
    fn engine(&self) -> Engine {
        Engine::Mssql
    }
    fn source_shutdown(&self) -> SourceShutdown {
        self.0.shutdown
    }
    async fn probe(&self) -> Result<ServerInfo> {
        Ok(self.0.snapshots.requested.server.clone())
    }
    async fn snapshot(&self, _: &str, _: &SnapshotOptions) -> Result<Snapshot> {
        panic!("ordinary scans must use the same bounded batch acquisition")
    }
    async fn snapshot_populations(&self, _: &str, options: &SnapshotOptions) -> Result<PopulationSnapshots> {
        self.0.acquisitions.fetch_add(1, Ordering::SeqCst);
        if self.0.block_snapshots.load(Ordering::SeqCst) {
            self.0.snapshot_entered.notify_one();
            return std::future::pending().await;
        }
        if let Some(selected) = &options.workload_population {
            let requested = self
                .0
                .snapshots
                .populations
                .iter()
                .find(|snapshot| {
                    snapshot.options.as_ref().and_then(|options| options.workload_population.as_ref()) == Some(selected)
                })
                .cloned()
                .ok_or_else(|| Error::invalid("unknown fixture population"))?;
            return Ok(PopulationSnapshots {
                requested,
                populations: vec![],
                catalog: self.0.snapshots.catalog.clone(),
            });
        }
        Ok(self.0.snapshots.clone())
    }
    async fn deadlocks(&self, _: f64) -> Result<culprit_core::DeadlockReport> {
        self.0.deadlocks.fetch_add(1, Ordering::SeqCst);
        Err(Engine::Mssql.refused(culprit_core::Ability::Deadlocks))
    }
    async fn recorded_changes(&self, _: &str) -> Result<Vec<culprit_core::ChangeEvent>> {
        self.0.events.fetch_add(1, Ordering::SeqCst);
        Ok(vec![])
    }
    async fn explain(&self, _: &str, _: ExplainMode) -> Result<Plan> {
        panic!("no application SQL is executed")
    }
    async fn live(&self) -> Result<LiveState> {
        panic!("no activity reads are needed for pure population analysis")
    }
    async fn run(&self, _: &str, _: usize) -> Result<culprit_core::ResultSet> {
        panic!("population acquisition never executes application SQL")
    }
}

struct OwnerCheck(Arc<NativeState>);
impl Check for OwnerCheck {
    fn info(&self) -> CheckInfo {
        CheckInfo {
            id: "fixture.owner".into(),
            title: "Native lifecycle".into(),
            category: Category::Configuration,
            engines: vec![Engine::Mssql],
            requires: vec![],
            description: "Proves the source owner has ended".into(),
        }
    }
    fn evidence(&self, _: &Snapshot) -> CheckEvidence {
        CheckEvidence::Available
    }
    fn run(&self, snapshot: &Snapshot) -> Vec<Finding> {
        assert_eq!(self.0.active.load(Ordering::SeqCst), 0, "analysis must not retain a provider");
        self.0
            .analyzed
            .lock()
            .unwrap()
            .push(snapshot.workload.as_ref().and_then(|workload| workload.population.clone()));
        let mut finding = culprit_checks::analyze(snapshot.clone()).findings.into_iter().next().unwrap();
        finding.id = "fixture.owner:shared-id".into();
        finding.check = "fixture.owner".into();
        finding.subject.kind = culprit_core::SubjectKind::Query;
        finding.title = match snapshot.workload.as_ref().and_then(|workload| workload.population.as_ref()) {
            Some(population) => format!("Group {} evidence", population.id),
            None => "Overview evidence".into(),
        };
        finding.fix = None;
        vec![finding]
    }
}

fn snapshots() -> PopulationSnapshots {
    let mut requested: Snapshot = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.json")).unwrap();
    let identity = DatabaseIdentity::server(Engine::Mssql, &requested.server.database, "fixture.invalid", 1433, None);
    let catalog = RuntimePopulationCatalog {
        membership: culprit_core::RuntimePopulationMembership::NativeCatalog,
        source: culprit_core::source_id::QUERY_STORE.into(),
        partitioned: true,
        requires_selection: true,
        populations: ["1", "7"]
            .map(|id| RuntimePopulation {
                identity: NativeIdentity::new(id),
                label: Message::from("fixture group"),
                native_properties: Default::default(),
            })
            .into(),
    };
    requested.options = Some(options());
    requested.runtime_populations = Some(catalog.clone());
    let mut workload = requested.workload.take().unwrap();
    requested.bind_identity(identity.clone());
    workload.window_start = Some("2026-11-01T00:00:00Z".into());
    workload.window_end = Some("2026-11-01T01:00:00Z".into());
    workload.window_hours = 1.0;
    workload.timeline = None;
    workload.population_catalog = Some(catalog.clone());
    let populations = catalog
        .populations
        .iter()
        .map(|population| {
            let mut snapshot = requested.clone();
            snapshot.options.as_mut().unwrap().workload_population = Some(population.identity.clone());
            let mut workload = workload.clone();
            workload.population = Some(population.identity.clone());
            snapshot.workload = Some(workload);
            snapshot.bind_identity(identity.clone());
            snapshot
        })
        .collect();
    PopulationSnapshots { requested, populations, catalog: Some(catalog) }
}
fn options() -> SnapshotOptions {
    SnapshotOptions { window_minutes: 60, ..Default::default() }
}

fn workspace(
    snapshots: PopulationSnapshots,
    shutdown: SourceShutdown,
) -> (tempfile::TempDir, Workspace, Arc<NativeState>) {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(NativeState {
        snapshots,
        shutdown,
        block_snapshots: AtomicBool::new(false),
        snapshot_entered: tokio::sync::Notify::new(),
        active: AtomicUsize::new(0),
        acquisitions: AtomicUsize::new(0),
        factories: AtomicUsize::new(0),
        deadlocks: AtomicUsize::new(0),
        events: AtomicUsize::new(0),
        analyzed: Mutex::new(vec![]),
    });
    let source = state.clone();
    let workspace = Workspace::open_in(home.path())
        .with_provider(
            Engine::Mssql,
            ProviderRegistration::new(
                Arc::new(move |_, _context| {
                    source.factories.fetch_add(1, Ordering::SeqCst);
                    source.active.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::new(NativeProvider(source.clone())) as Box<dyn Provider>)
                }),
                culprit_engine::ProviderConstructionPolicy::DeferredNativeWork,
                vec![],
            )
            .unwrap(),
        )
        .with_check(Arc::new(OwnerCheck(state.clone())));
    workspace
        .add_connection(NewConnection {
            name: "shop".into(),
            input: "mssql://reader@fixture.invalid/shop".into(),
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap();
    (home, workspace, state)
}

#[tokio::test]
async fn ordinary_scan_analyzes_all_groups_once_and_keeps_the_default_cache_independent() {
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let report = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan("shop", &options(), operation).await
    })
    .await
    .unwrap();
    assert!(report.snapshot.workload.is_none());
    assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
    assert_eq!(state.deadlocks.load(Ordering::SeqCst), 1);
    assert_eq!(state.events.load(Ordering::SeqCst), 1);
    assert_eq!(*state.analyzed.lock().unwrap(), [None, Some(NativeIdentity::new("1")), Some(NativeIdentity::new("7"))]);
    let default = workspace.latest_report("shop", Some(60)).unwrap().unwrap();
    assert_eq!(default.snapshot.id, report.snapshot.id);
    let mut ids = std::collections::BTreeSet::from([report.snapshot.id.clone()]);
    for id in ["1", "7"] {
        let selected =
            workspace.latest_population_report("shop", Some(60), Some(&NativeIdentity::new(id))).unwrap().unwrap();
        assert!(ids.insert(selected.snapshot.id.clone()));
        assert_eq!(selected.snapshot.taken_at, report.snapshot.taken_at);
        assert_eq!(selected.snapshot.options.as_ref().unwrap().workload_population, Some(NativeIdentity::new(id)));
        assert_eq!(selected.snapshot.workload.as_ref().unwrap().population, Some(NativeIdentity::new(id)));
    }
    assert_eq!(workspace.report_history("shop").unwrap().len(), 3);
    assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1, "cache rereads cannot open a source");
}

#[tokio::test]
async fn explicit_selection_acquires_and_publishes_only_the_requested_group() {
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let selected = SnapshotOptions { workload_population: Some(NativeIdentity::new("7")), ..options() };
    let reports = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_populations("shop", &selected, operation).await
    })
    .await
    .unwrap();
    assert!(reports.populations.is_empty());
    assert_eq!(reports.requested.snapshot.workload.as_ref().unwrap().population, selected.workload_population);
    assert!(workspace.latest_report("shop", None).unwrap().is_none());
    assert!(workspace.latest_population_report("shop", None, Some(&NativeIdentity::new("1"))).unwrap().is_none());
    assert_eq!(workspace.report_history("shop").unwrap().len(), 1);
    assert_eq!(*state.analyzed.lock().unwrap(), [Some(NativeIdentity::new("7"))]);
}

#[tokio::test]
async fn invalid_batch_membership_is_rejected_before_auxiliary_reads_analysis_or_persistence() {
    let mut batch = snapshots();
    batch.populations[1] = batch.populations[0].clone();
    let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
    let error = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_populations("shop", &options(), operation).await
    })
    .await
    .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
    assert_eq!(state.deadlocks.load(Ordering::SeqCst), 0);
    assert_eq!(state.events.load(Ordering::SeqCst), 0);
    assert!(state.analyzed.lock().unwrap().is_empty());
    assert!(workspace.report_history("shop").unwrap().is_empty());
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unconfirmed_native_shutdown_cannot_publish_any_population() {
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Unconfirmed);
    let error = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_populations("shop", &options(), operation).await
    })
    .await
    .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert!(state.analyzed.lock().unwrap().is_empty());
    assert!(workspace.latest_report("shop", None).unwrap().is_none());
    assert!(workspace.report_history("shop").unwrap().is_empty());
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn exact_report_actions_cannot_export_or_verify_the_default_view_with_the_same_finding_id() {
    use culprit_engine::export::{ExportFormat, ExportOptions};
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let reports = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_populations("shop", &options(), operation).await
    })
    .await
    .unwrap();
    let selected = reports
        .populations
        .iter()
        .find(|report| report.snapshot.options.as_ref().unwrap().workload_population == Some(NativeIdentity::new("7")))
        .unwrap();
    let id = "fixture.owner:shared-id".to_owned();
    let exported = workspace
        .export_finding_at("shop", &selected.snapshot.id, &id, ExportFormat::Markdown, &ExportOptions::default())
        .unwrap();
    assert!(exported.contents.contains("Group 7 evidence"));
    assert!(!exported.contents.contains("Overview evidence"));
    assert!(
        workspace.export_finding_at("shop", "missing", &id, ExportFormat::Markdown, &ExportOptions::default()).is_err()
    );
    let verified = workspace.verify_at("shop", &selected.snapshot.id, &[id]).await.unwrap();
    assert_eq!(verified.before_report_id.as_deref(), Some(selected.snapshot.id.as_str()));
    let exact_after = workspace.report_at("shop", verified.after_report_id.as_deref().unwrap()).unwrap();
    assert_eq!(exact_after.snapshot.workload.as_ref().unwrap().population, Some(NativeIdentity::new("7")));
    assert_eq!(verified.findings[0].title, "Group 7 evidence");
    let after = workspace.latest_population_report("shop", Some(60), Some(&NativeIdentity::new("7"))).unwrap().unwrap();
    assert_eq!(after.snapshot.workload.as_ref().unwrap().population, Some(NativeIdentity::new("7")));
    assert_eq!(state.acquisitions.load(Ordering::SeqCst), 2);
    assert_eq!(workspace.latest_report("shop", Some(60)).unwrap().unwrap().snapshot.id, reports.requested.snapshot.id);
}

#[tokio::test]
async fn a_failed_explicit_group_cannot_borrow_an_application_logs_unpartitioned_timings() {
    let mut batch = snapshots();
    batch.populations[1].workload = None;
    let (home, workspace, _) = workspace(batch, SourceShutdown::Quiescent);
    let selected = SnapshotOptions { workload_population: Some(NativeIdentity::new("7")), ..options() };
    let missing_log = home.path().join("must-not-be-opened.jsonl");
    let report = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_with_log("shop", &selected, Some(&missing_log), operation).await
    })
    .await
    .unwrap();
    assert!(report.snapshot.workload.is_none());
    assert!(!report.snapshot.sources.iter().any(|source| source.source == "query log"));
}

#[tokio::test]
async fn a_selected_or_automatic_group_cannot_borrow_another_connections_last_actual_execution() {
    for automatic in [false, true] {
        let mut batch = snapshots();
        if automatic {
            let mut selected = batch.populations.pop().unwrap();
            batch.catalog.as_mut().unwrap().populations.retain(|population| population.identity.id == "7");
            batch.catalog.as_mut().unwrap().requires_selection = false;
            selected.runtime_populations = batch.catalog.clone();
            selected.workload.as_mut().unwrap().population_catalog = batch.catalog.clone();
            batch.requested = selected.clone();
            batch.requested.options.as_mut().unwrap().workload_population = None;
            batch.populations = vec![selected];
        }
        let (_home, workspace, _) = workspace(batch, SourceShutdown::Quiescent);
        let reports = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace.scan_populations("shop", &options(), operation).await
        })
        .await
        .unwrap();
        let report = if automatic { reports.requested } else { reports.populations.into_iter().last().unwrap() };
        let workspace = workspace.with_provider(
            Engine::Mssql,
            ProviderRegistration::new(
                Arc::new(|_, _context| {
                    panic!("unproven execution scope must be rejected before authentication/provider creation")
                }),
                culprit_engine::ProviderConstructionPolicy::DeferredNativeWork,
                vec![],
            )
            .unwrap(),
        );
        let query = &report.snapshot.workload.as_ref().unwrap().queries[0];
        let error = workspace
            .last_actual_plan_at("shop", &query.id, query.plan_id.as_deref(), Some(&report.snapshot.id))
            .await
            .unwrap_err();
        assert_eq!(error.code, culprit_core::ErrorCode::Unsupported);
        assert!(matches!(error.detail, Some(culprit_core::ErrorDetail::ActualPlanExecutionPopulationUnverified)));
    }
}
