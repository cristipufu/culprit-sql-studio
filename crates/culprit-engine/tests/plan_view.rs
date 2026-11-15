#[path = "support/plan_view_cases.rs"]
mod inputs;

use async_trait::async_trait;
use culprit_core::{
    Engine, Error, ExplainMode, LiveState, Plan, Provider, Result, ResultSet, ServerInfo, Snapshot, SnapshotOptions,
    telemetry::*,
};

#[derive(Clone)]
struct PlanReader {
    plan: Plan,
    provenance: RetainedPlanProvenance,
    native_plans: std::collections::BTreeMap<String, NativePlanIdentity>,
}

#[async_trait]
impl Provider for PlanReader {
    fn engine(&self) -> Engine {
        Engine::Mssql
    }
    async fn probe(&self) -> Result<ServerInfo> {
        Err(Error::invalid("not needed"))
    }
    async fn snapshot(&self, _: &str, _: &SnapshotOptions) -> Result<Snapshot> {
        Err(Error::invalid("not needed"))
    }
    async fn explain(&self, _: &str, _: ExplainMode) -> Result<Plan> {
        Ok(self.plan.clone())
    }
    async fn live(&self) -> Result<LiveState> {
        Err(Error::invalid("not needed"))
    }
    async fn run(&self, _: &str, _: usize) -> Result<ResultSet> {
        Err(Error::invalid("not needed"))
    }
    async fn stored_plan(&self, _: &str, _: &str) -> Result<Option<Plan>> {
        Ok(Some(self.plan.clone()))
    }
    async fn last_actual_plan(&self, request: &RetainedActualPlanRequest) -> Result<RetainedActualPlan> {
        request.validate()?;
        let mut plan = self.plan.clone();
        plan.kind = culprit_core::PlanKind::Actual;
        assert_eq!(request.query_id, self.provenance.query.as_ref().unwrap().query_id);
        let mut provenance = self.provenance.clone();
        provenance.native_plan = request.plan_id.as_ref().and_then(|id| self.native_plans.get(id)).cloned();
        Ok(RetainedActualPlan { plan, provenance })
    }

    async fn query_progress(&self, session: &str, _: bool) -> Result<culprit_core::QueryProgress> {
        let mut progress = culprit_core::QueryProgress::idle(session, Engine::Mssql, "2026-10-10T00:00:00Z".into());
        progress.running = true;
        progress.plan = Some(self.plan.clone());
        Ok(progress)
    }
}

fn known_retained_fixture() -> (inputs::Case, PlanReader, culprit_core::DatabaseIdentity) {
    let mut case = inputs::cases().into_iter().find(|case| case.name == "slow-variant").unwrap();
    let query = case.query.as_mut().unwrap();
    let native_query = NativeQueryIdentity {
        query_id: "7001".into(),
        qualifiers: [
            ("context_settings_id".into(), "5".into()),
            ("object_id".into(), "0".into()),
            ("query_hash".into(), "0123456789ABCDEF".into()),
            ("initial_compile_at".into(), "2026-10-09T06:00:00.000000000Z".into()),
        ]
        .into(),
    };
    query.native_identity = Some(native_query.clone());
    let mut native_plans = std::collections::BTreeMap::new();
    for (index, variant) in query.plans.iter_mut().enumerate() {
        let identity = NativePlanIdentity {
            plan_id: (9001 + index).to_string(),
            query: native_query.clone(),
            qualifiers: [
                ("plan_hash".into(), "0011223344556677".into()),
                ("initial_compile_at".into(), "2026-10-09T06:00:00.000000000Z".into()),
            ]
            .into(),
        };
        variant.native_identity = Some(identity.clone());
        native_plans.insert(identity.plan_id.clone(), identity);
    }
    query.native_plan_identity = query
        .plans
        .iter()
        .find(|variant| Some(&variant.id) == query.plan_id.as_ref())
        .and_then(|variant| variant.native_identity.clone());
    let mut target =
        culprit_core::DatabaseIdentity::server(Engine::Mssql, "culprit_demo", "replay.invalid", 1433, None);
    target.incarnation = Some("owned-test-database-incarnation".into());
    let provenance = RetainedPlanProvenance {
        read: NativeReadContext {
            target: target.clone().into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: Engine::Mssql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("owned-test-machine"),
                    native: NativeIdentity::new("owned-test-instance"),
                }),
                epoch: Some("owned-test-process-epoch".into()),
            },
            observed_from: "2026-10-10T00:00:00Z".into(),
            observed_until: "2026-10-10T00:00:01Z".into(),
            database_writable: Some(true),
        },
        statement: CachedStatementContext {
            identity: CachedStatementIdentity {
                plan_handle: "0x0102".into(),
                sql_handle: "0x0304".into(),
                statement_start_bytes: 0,
                statement_end_bytes: -1,
                created_without_offset: "2026-10-09T06:00:00.003".into(),
                plan_generation: "1".into(),
            },
            database_id: 7,
            query_hash: None,
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            source_text_bytes: None,
            source_text_complete: false,
        },
        query: Some(native_query),
        native_plan: None,
    };
    let reader = PlanReader { plan: case.plan.clone(), provenance, native_plans };
    (case, reader, target)
}

#[tokio::test]
async fn workspace_plan_endpoints_project_the_selected_variant_and_execution_state() {
    let home = std::env::temp_dir().join(format!("culprit-plan-view-{}", std::process::id()));
    let (mut case, mut reader, target) = known_retained_fixture();
    let query = case.query.take().unwrap();
    let query_id = query.id.clone();
    // Deliberately stale input: all endpoints must replace it with their own context.
    case.plan.view.as_mut().unwrap().cost_note = Some("obsolete".into());
    reader.plan = case.plan.clone();
    let ws = culprit_engine::Workspace::open_in(&home).with_provider(
        Engine::Mssql,
        culprit_engine::ProviderRegistration::new(
            std::sync::Arc::new(move |_| Ok(Box::new(reader.clone()))),
            Vec::new(),
        )
        .unwrap(),
    );
    ws.add_connection(culprit_engine::NewConnection {
        name: "replayed".into(),
        input: "mssql://reader@replay.invalid/culprit_demo".into(),
        auth: Some(culprit_core::AuthKind::None),
        environment: culprit_core::Environment::Development,
        ..Default::default()
    })
    .unwrap();
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.connection = "replayed".into();
    snapshot.identity = Some(target);
    snapshot.workload.as_mut().unwrap().queries = vec![query];
    let report = culprit_checks::analyze(snapshot);
    culprit_engine::reports::save(&ws.paths().reports_dir("replayed"), &report).unwrap();
    for id in ["slow", "fast", "unmeasured"] {
        {
            let plan = ws.query_plan_at("replayed", &query_id, Some(id), Some(&report.snapshot.id)).await.unwrap();
            let view = plan.view.unwrap();
            if id == "unmeasured" {
                assert!(view.query.is_none());
                assert!(plan.insights.unwrap().note.unwrap().contains("No matching measurements"));
            } else {
                assert_eq!(view.query.unwrap().plan_id.as_deref(), Some(id));
                let note = view.cost_note.unwrap_or_default();
                assert_eq!(note.contains("322 pages a run"), id == "slow", "{id}: {note}");
                assert_ne!(note, "obsolete");
            }
        }
        if id == "unmeasured" {
            let error =
                ws.last_actual_plan_at("replayed", &query_id, Some(id), Some(&report.snapshot.id)).await.unwrap_err();
            assert!(matches!(error.detail, Some(culprit_core::ErrorDetail::ActualPlanIdentityUnverified)));
            continue;
        }
        let retained =
            ws.last_actual_plan_at("replayed", &query_id, Some(id), Some(&report.snapshot.id)).await.unwrap();
        assert!(retained.view.as_ref().unwrap().query.is_none());
        assert!(retained.facts().retained.is_some());
        assert!(retained.insights.as_ref().unwrap().items.iter().all(|item| item.fix_sql.is_none()));
        let note = retained.view.as_ref().unwrap().cost_note.as_deref().unwrap_or_default();
        assert!(!note.contains("322 pages a run") && !note.contains("20 ms"));
    }
    let explained = ws.explain("replayed", "SELECT 1", ExplainMode::Estimated, false).await.unwrap();
    assert!(explained.view.is_some());
    assert!(explained.view.unwrap().query.is_none());
    let progress = ws.query_progress("replayed", "52", true).await.unwrap().plan.unwrap();
    let view = progress.view.unwrap();
    assert_eq!(view.kind, culprit_core::PlanKind::Estimated);
    assert!(view.actual.is_none());
    std::fs::remove_dir_all(home).unwrap();
}

fn retained_workspace(
    query: culprit_core::QueryStat,
    target: Option<culprit_core::DatabaseIdentity>,
    reader: Option<PlanReader>,
) -> (tempfile::TempDir, culprit_engine::Workspace, culprit_core::Report) {
    let directory = tempfile::tempdir().unwrap();
    let workspace = culprit_engine::Workspace::open_in(directory.path()).with_provider(
        Engine::Mssql,
        culprit_engine::ProviderRegistration::new(
            std::sync::Arc::new(move |_| match &reader {
                Some(reader) => Ok(Box::new(reader.clone())),
                None => panic!("unverified retained-plan identity must fail before provider creation"),
            }),
            Vec::new(),
        )
        .unwrap(),
    );
    workspace
        .add_connection(culprit_engine::NewConnection {
            name: "replayed".into(),
            input: "mssql://reader@replay.invalid/culprit_demo".into(),
            auth: Some(culprit_core::AuthKind::None),
            environment: culprit_core::Environment::Development,
            ..Default::default()
        })
        .unwrap();
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.connection = "replayed".into();
    snapshot.identity = target;
    snapshot.workload.as_mut().unwrap().queries = vec![query];
    let report = culprit_checks::analyze(snapshot);
    culprit_engine::reports::save(&workspace.paths().reports_dir("replayed"), &report).unwrap();
    (directory, workspace, report)
}

#[tokio::test]
async fn missing_report_query_plan_or_target_identity_refuses_retained_reads_before_provider_creation() {
    enum MissingIdentity {
        Query,
        Plan,
        Target,
    }
    for missing in [MissingIdentity::Query, MissingIdentity::Plan, MissingIdentity::Target] {
        let (mut case, _, target) = known_retained_fixture();
        let mut query = case.query.take().unwrap();
        let mut target = Some(target);
        match missing {
            MissingIdentity::Query => query.native_identity = None,
            MissingIdentity::Plan => {
                query.native_plan_identity = None;
                query.plans.iter_mut().find(|variant| variant.id == "slow").unwrap().native_identity = None;
            }
            MissingIdentity::Target => target = None,
        }
        let query_id = query.id.clone();
        let (_directory, workspace, report) = retained_workspace(query, target, None);
        let error = workspace
            .last_actual_plan_at("replayed", &query_id, Some("slow"), Some(&report.snapshot.id))
            .await
            .unwrap_err();
        assert!(matches!(error.detail, Some(culprit_core::ErrorDetail::ActualPlanIdentityUnverified)));
    }
}

#[tokio::test]
async fn an_engine_without_retained_plans_keeps_its_capability_refusal_without_requiring_native_lookup_proof() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = culprit_engine::Workspace::open_in(directory.path()).with_provider(
        Engine::Postgres,
        culprit_engine::ProviderRegistration::new(
            std::sync::Arc::new(|_| panic!("a refused capability cannot create a native provider")),
            Vec::new(),
        )
        .unwrap(),
    );
    workspace
        .add_connection(culprit_engine::NewConnection {
            name: "replayed".into(),
            input: "postgres://reader@replay.invalid/sample".into(),
            auth: Some(culprit_core::AuthKind::None),
            ..Default::default()
        })
        .unwrap();
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../fixtures/postgres-demo.json")).unwrap();
    snapshot.connection = "replayed".into();
    let query_id = snapshot.queries()[0].id.clone();
    assert!(snapshot.queries()[0].native_identity.is_none());
    let report = culprit_checks::analyze(snapshot);
    culprit_engine::reports::save(&workspace.paths().reports_dir("replayed"), &report).unwrap();
    let error =
        workspace.last_actual_plan_at("replayed", &query_id, None, Some(&report.snapshot.id)).await.unwrap_err();
    assert_eq!(error.code, culprit_core::ErrorCode::Unsupported);
    assert!(error.detail.is_none(), "the normal unsupported capability still permits the UI's measured-run path");
    assert_eq!(error.message, Engine::Postgres.refused(culprit_core::Ability::LastActualPlan).message);
}

#[tokio::test]
async fn reused_native_query_plan_ids_or_a_different_current_database_do_not_authorize_retained_output() {
    enum ChangedIdentity {
        QueryContext,
        PlanGeneration,
        Target,
    }
    for changed in [ChangedIdentity::QueryContext, ChangedIdentity::PlanGeneration, ChangedIdentity::Target] {
        let (mut case, mut reader, target) = known_retained_fixture();
        match changed {
            ChangedIdentity::QueryContext => {
                let query = reader.provenance.query.as_mut().unwrap();
                query.qualifiers.insert("context_settings_id".into(), "6".into());
                for plan in reader.native_plans.values_mut() {
                    plan.query = query.clone();
                }
            }
            ChangedIdentity::PlanGeneration => {
                for plan in reader.native_plans.values_mut() {
                    plan.qualifiers.insert("initial_compile_at".into(), "2026-10-09T07:00:00.000000000Z".into());
                }
            }
            ChangedIdentity::Target => {
                let mut different = target.clone();
                different.incarnation = Some("different-current-database-incarnation".into());
                reader.provenance.read.target = different.into();
            }
        }
        let query = case.query.take().unwrap();
        let query_id = query.id.clone();
        let (_directory, workspace, report) = retained_workspace(query, Some(target), Some(reader));
        let error = workspace
            .last_actual_plan_at("replayed", &query_id, Some("slow"), Some(&report.snapshot.id))
            .await
            .unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    }
}

#[test]
fn measured_variant_context_and_outliers_survive_the_shared_projection() {
    let cases = inputs::cases();
    let view = |name: &str| cases.iter().find(|c| c.name == name).unwrap().plan.view.as_ref().unwrap();
    assert!(view("wide-read").cost_note.as_ref().unwrap().contains("72 pages a run"));
    assert!(view("slow-variant").cost_note.as_ref().unwrap().contains("322 pages a run"));
    let fast = view("fast-variant").cost_note.as_ref().unwrap();
    assert!(!fast.contains("322 pages") && !fast.contains("20 ms"));
    assert!(view("slow-duration").cost_note.as_ref().unwrap().contains("averaged 20 ms"));
    let outlier = view("outlier").cost_note.as_ref().unwrap();
    assert!(outlier.contains("4 runs took 2.4 s in all, with the slowest taking 2.4 s"));
    let small = &view("small-scan").nodes[0];
    assert!(small.no_share_reason.as_ref().unwrap().contains("as a whole is cheap"));
    assert!(small.finding_no_share_reason.as_ref().unwrap().contains("all 3.9K rows of shop.widgets"));
}

#[test]
fn actual_facts_share_one_clock_and_keep_missing_values_unknown() {
    let cases = inputs::cases();
    let view = |name: &str| cases.iter().find(|c| c.name == name).unwrap().plan.view.as_ref().unwrap();
    let triggers = view("triggers").actual.as_ref().unwrap();
    assert_eq!(
        (triggers.elapsed_ms, triggers.memory_granted_kb, triggers.memory_used_kb),
        (Some(10.0), Some(1024.0), None)
    );
    assert_eq!((triggers.triggers.len(), triggers.triggers[0].time_ms), (1, 120.5));
    let rows = view("rows-only");
    assert_eq!(rows.timed_total_ms, None);
    let actual = rows.actual.as_ref().unwrap();
    assert!(!actual.timed);
    assert_eq!((actual.elapsed_ms, actual.cpu_ms), (None, None));
    assert!(rows.states_shares, "unknown zero time must not claim a costed plan was measured cheap");
    let batch = view("batch-mode");
    assert_eq!(batch.timed_total_ms, Some(47.0));
    assert_eq!(batch.nodes.iter().find(|n| n.id == 2).unwrap().own_ms, Some(15.0));
    let compute = batch.nodes.iter().find(|n| n.id == 1).unwrap();
    assert_eq!((compute.own_ms, compute.time_share), (None, None));
    assert!(batch.time_note.as_ref().unwrap().contains("untimed"));
    let worst = view("estimate-origin").actual.as_ref().unwrap();
    assert_eq!((worst.worst_estimate_node, worst.worst_estimate), (Some(1), Some(5000.0)));
}

#[test]
fn partial_execution_is_not_projected_as_a_completed_actual_run() {
    let cases = inputs::cases();
    let progress = cases.iter().find(|c| c.name == "in-progress").unwrap();
    assert_eq!(progress.plan.kind, culprit_core::PlanKind::Actual, "the collected plan stays intact");
    let view = progress.plan.view.as_ref().unwrap();
    assert_eq!(view.kind, culprit_core::PlanKind::Estimated);
    assert!(view.actual.is_none());
    assert!(view.timed_total_ms.is_none());
}

#[test]
fn imported_plan_json_cannot_supply_a_stale_or_forged_projection() {
    let mut case = inputs::cases().remove(0);
    case.plan.view.as_mut().unwrap().cost_note = Some("A stale explanation".into());
    case.plan.view.as_mut().unwrap().nodes[0].cost_share = Some(999.0);
    let text = serde_json::to_string(&case.plan).unwrap();
    let parsed = culprit_engine::plans::parse_plan(&text).unwrap();
    let view = parsed.view.unwrap();
    assert_ne!(view.cost_note.as_deref(), Some("A stale explanation"));
    assert_ne!(view.nodes[0].cost_share, Some(999.0));

    // Cached projections can change shape between analyzer versions. Only raw plan facts are input.
    for cached in [serde_json::json!({"old_schema": true}), serde_json::json!("obsolete"), serde_json::Value::Null] {
        let mut value = serde_json::to_value(&case.plan).unwrap();
        value["view"] = cached;
        let parsed = culprit_engine::plans::parse_plan(&value.to_string()).unwrap();
        assert!(parsed.view.is_some(), "the current projection must be recomputed");
        assert_eq!(parsed.root.id, case.plan.root.id);
    }
}

#[test]
fn projected_runs_keep_zero_and_parallel_execution_semantics() {
    let mut plan = inputs::cases().remove(0).plan;
    plan.root.act_executions = Some(0.0);
    let view = culprit_checks::plan_view(&plan, None);
    assert_eq!((view.nodes[0].runs, view.nodes[0].estimated_rows_for_runs), (Some(0.0), Some(0.0)));
    plan.root.act_executions = Some(4.0);
    plan.root.facts_mut().parallel_threads = Some(4);
    let view = culprit_checks::plan_view(&plan, None);
    assert_eq!((view.nodes[0].runs, view.nodes[0].estimated_rows_for_runs), (Some(1.0), Some(plan.root.est_rows)));
}

#[test]
fn browser_plan_view_contracts_match_current_rust_analysis() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/plan-view.sample.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    crate::invariants::assert_browser_json(
        &saved,
        &serde_json::to_value(inputs::cases()).unwrap(),
        "refresh plan view samples",
    );

    let actual: serde_json::Value = serde_json::from_str(include_str!("../../../fixtures/actual.sample.json")).unwrap();
    for queries in actual["last_actual"].as_object().unwrap().values() {
        for saved in queries.as_object().unwrap().values() {
            let plan: Plan = serde_json::from_value(saved.clone()).unwrap();
            crate::invariants::assert_browser_json(
                &saved["view"],
                &serde_json::to_value(culprit_checks::plan_view(&plan, None)).unwrap(),
                "refresh recorded actual plan projections",
            );
        }
    }
    for frames in actual["progress"].as_object().unwrap().values() {
        for frame in frames.as_array().unwrap().iter().filter(|f| !f["plan"].is_null()) {
            let mut plan: Plan = serde_json::from_value(frame["plan"].clone()).unwrap();
            culprit_checks::present_progress_plan(&mut plan);
            crate::invariants::assert_browser_json(
                &frame["plan"]["view"],
                &serde_json::to_value(plan.view).unwrap(),
                "refresh recorded progress projections",
            );
        }
    }
}
