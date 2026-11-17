use super::*;
use crate::tuning::TuningAdviceCheck;
use culprit_core::{
    Check, ForcedPlan, QueryHint, RuntimePopulation, RuntimePopulationCatalog, TuningAdvice, TuningRecommendation,
    i18n::Message, telemetry::NativeIdentity,
};

fn snapshot() -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!("../../../../../../fixtures/mssql-demo.json")).unwrap();
    // This synthetic native source explicitly reports an unpartitioned runtime layout.
    snapshot.workload.as_mut().unwrap().population_catalog = Some(RuntimePopulationCatalog {
        membership: culprit_core::RuntimePopulationMembership::NativeCatalog,
        source: culprit_core::source_id::QUERY_STORE.into(),
        partitioned: false,
        requires_selection: false,
        populations: Vec::new(),
    });
    snapshot.tuning = Some(TuningAdvice { plan_scope: QueryStoreScope::Local, ..Default::default() });
    snapshot
}

fn select(snapshot: &mut Snapshot, group: &str) {
    let workload = snapshot.workload.as_mut().unwrap();
    workload.population = Some(NativeIdentity::new(group));
    workload.population_catalog = Some(RuntimePopulationCatalog {
        membership: culprit_core::RuntimePopulationMembership::NativeCatalog,
        source: culprit_core::source_id::QUERY_STORE.into(),
        partitioned: true,
        requires_selection: true,
        populations: ["7", "8"]
            .into_iter()
            .map(|id| RuntimePopulation {
                identity: NativeIdentity::new(id),
                label: Message::from(id),
                native_properties: Default::default(),
            })
            .collect(),
    });
}

fn findings(snapshot: &Snapshot) -> Vec<culprit_core::Finding> {
    TuningAdviceCheck.run(snapshot)
}

fn force_snapshot() -> Snapshot {
    let mut snapshot = snapshot();
    let advice = snapshot.tuning.as_mut().unwrap();
    advice.recommendations.push(TuningRecommendation {
        name: "PR_9".into(),
        kind: "FORCE_LAST_GOOD_PLAN".into(),
        state: Some("Active".into()),
        query_id: Some("9".into()),
        recommended_plan_id: Some("9".into()),
        regressed_plan_id: Some("3".into()),
        estimated_gain: Some(0.75),
        script: Some("EXEC native-unscoped-command".into()),
        ..Default::default()
    });
    advice.forced_plans.push(ForcedPlan { query_id: "9".into(), plan_id: "9".into(), ..Default::default() });
    let mut index = snapshot.indexes.iter().find(|index| index.name == "ix_order_items_product_id").unwrap().clone();
    index.name = "ix_order_items_order_id".into();
    index.columns = vec!["order_id".into()];
    snapshot.indexes.push(index);
    let query = snapshot.workload.as_mut().unwrap().queries.iter_mut().find(|query| query.id == "9").unwrap();
    query.plan.as_mut().unwrap().root.children[0].est_rows = 1.8;
    snapshot
}

#[test]
fn only_positive_matching_native_population_authorizes_plan_runtime_joins() {
    let base = snapshot();
    assert!(plan_workload(&base).is_some());
    for scope in [QueryStoreScope::Unknown, QueryStoreScope::ReplicaGroup { id: "7".into() }] {
        let mut snapshot = base.clone();
        snapshot.tuning.as_mut().unwrap().plan_scope = scope;
        assert!(plan_workload(&snapshot).is_none());
    }
    let mut absent = base.clone();
    absent.workload.as_mut().unwrap().population_catalog = None;
    assert!(plan_workload(&absent).is_none(), "primary mode and None population are not unpartitioned proof");
    let mut foreign = base.clone();
    foreign.workload.as_mut().unwrap().population_catalog.as_mut().unwrap().source = "other-native-source".into();
    assert!(plan_workload(&foreign).is_none(), "display source names cannot qualify another native catalog");
    foreign.workload.as_mut().unwrap().population_catalog = base.workload.as_ref().unwrap().population_catalog.clone();
    foreign.workload.as_mut().unwrap().kind = Some(WorkloadKind::PlanCache);
    assert!(plan_workload(&foreign).is_none(), "a retained Query Store catalog is not cache-population proof");
    let mut selected = base;
    select(&mut selected, "7");
    assert!(plan_workload(&selected).is_none(), "Local forcing never describes a selected replica group");
    selected.tuning.as_mut().unwrap().plan_scope = QueryStoreScope::ReplicaGroup { id: "7".into() };
    assert!(plan_workload(&selected).is_some());
    selected.tuning.as_mut().unwrap().plan_scope = QueryStoreScope::ReplicaGroup { id: "8".into() };
    assert!(plan_workload(&selected).is_none());
    selected.tuning.as_mut().unwrap().plan_scope = QueryStoreScope::ReplicaGroup { id: "7".into() };
    selected.workload.as_mut().unwrap().population = Some(NativeIdentity::new("7").qualified("other", "context"));
    assert!(plan_workload(&selected).is_none(), "matching text cannot remove native identity qualifiers");
}

#[test]
fn unknown_global_forcing_keeps_native_facts_without_borrowing_selected_group_cost_or_scan() {
    let mut snapshot = force_snapshot();
    assert!(findings(&snapshot).iter().any(|finding| finding.id == "tuning-advice:forced-scan:9"));
    select(&mut snapshot, "7");
    snapshot.tuning.as_mut().unwrap().plan_scope = QueryStoreScope::Unknown;
    snapshot.tuning.as_mut().unwrap().forced_plans.push(ForcedPlan {
        query_id: "7".into(),
        plan_id: "1".into(),
        failures: 34.0,
        ..Default::default()
    });
    let findings = findings(&snapshot);
    assert_eq!(findings.len(), 3);
    assert!(!findings.iter().any(|finding| finding.id == "tuning-advice:forced-scan:9"));
    for finding in &findings {
        assert!(finding.queries.is_empty());
        assert!(finding.impact.db_time_share.is_none());
        assert!(!finding.inactive);
        assert!(finding.fix.as_ref().unwrap().sql.is_none());
        assert!(!finding.verify.iter().any(|line| line.contains("EXEC ")));
    }
    let recommendation = findings.iter().find(|finding| finding.id == "tuning-advice:force:9").unwrap();
    assert_eq!(recommendation.facts.recommended_plan.as_ref().unwrap().plan, "9");
    assert!(recommendation.metrics.iter().any(|metric| metric.value == 0.75));
    assert!(findings.iter().any(|finding| finding.id == "tuning-advice:forced-failing:7"));
    assert!(findings.iter().any(|finding| finding.id == "tuning-advice:forced-plans"));
}

#[test]
fn exact_group_scope_preserves_measured_scan_and_targets_its_group_in_both_directions() {
    let mut snapshot = force_snapshot();
    select(&mut snapshot, "7");
    snapshot.tuning.as_mut().unwrap().plan_scope = QueryStoreScope::ReplicaGroup { id: "7".into() };
    let findings = findings(&snapshot);
    let scan = findings.iter().find(|finding| finding.id == "tuning-advice:forced-scan:9").unwrap();
    assert!(scan.impact.db_time_share.is_some());
    assert_eq!(scan.queries[0].id, "9");
    assert!(scan.fix.as_ref().unwrap().sql.as_ref().unwrap().contains("@replica_group_id = 7"));
    assert!(
        scan.verify
            .iter()
            .any(|line| line.contains("sp_query_store_force_plan") && line.contains("@replica_group_id = 7"))
    );
    let recommendation = findings.iter().find(|finding| finding.id == "tuning-advice:force:9").unwrap();
    let sql = recommendation.fix.as_ref().unwrap().sql.as_ref().unwrap();
    assert!(sql.contains("@replica_group_id = 7"));
    assert!(!sql.contains("native-unscoped-command"), "a source script cannot silently default to primary");
}

#[test]
fn readable_secondary_keeps_same_group_measurements_but_never_supplies_local_management_sql() {
    let mut snapshot = force_snapshot();
    select(&mut snapshot, "7");
    snapshot.tuning.as_mut().unwrap().plan_scope = QueryStoreScope::ReplicaGroup { id: "7".into() };
    for mode in ["READ_CAPTURE_SECONDARY", "READ_ONLY", "unknown"] {
        snapshot
            .settings
            .iter_mut()
            .find(|setting| setting.name == "query_store_options.actual_state_desc")
            .unwrap()
            .value = mode.into();
        assert!(plan_workload(&snapshot).is_some());
        for finding in findings(&snapshot) {
            assert!(finding.fix.as_ref().unwrap().sql.is_none());
            assert!(!finding.verify.iter().any(|line| line.contains("EXEC ")));
        }
    }
}

#[test]
fn explicit_hint_group_is_not_erased_by_unknown_plan_forcing_scope() {
    let mut snapshot = snapshot();
    select(&mut snapshot, "7");
    let tuning = snapshot.tuning.as_mut().unwrap();
    tuning.plan_scope = QueryStoreScope::Unknown;
    tuning.hints.push(QueryHint {
        query_id: "9".into(),
        hint_id: "3".into(),
        scope: QueryStoreScope::ReplicaGroup { id: "8".into() },
        hint: "OPTION(MAXDOP 1)".into(),
        failures: 3.0,
        ..Default::default()
    });
    let findings = findings(&snapshot);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "tuning-advice:hint-failing:9:replica:8");
    assert!(findings[0].fix.as_ref().unwrap().sql.as_ref().unwrap().contains("@replica_group_id = 8"));
    assert!(findings[0].queries.is_empty());
}

fn index_recommendation(kind: &str) -> TuningRecommendation {
    TuningRecommendation {
        name: "IR_native".into(),
        kind: kind.into(),
        state: Some("Active".into()),
        score: Some(64.0),
        script: Some(
            "CREATE NONCLUSTERED INDEX [native_script_canary] ON [shop].[orders] ([channel],[created_at])".into(),
        ),
        index: Some(culprit_core::RecommendedIndex {
            schema: "shop".into(),
            table: "orders".into(),
            name: Some(if kind == "DropIndex" { "ix_orders_channel" } else { "native_script_canary" }.into()),
            columns: if kind == "DropIndex" { Vec::new() } else { vec!["channel".into(), "created_at".into()] },
            include: Vec::new(),
        }),
        ..Default::default()
    }
}

#[test]
fn database_wide_index_recommendations_do_not_borrow_selected_group_queries_or_costs() {
    let mut snapshot = snapshot();
    snapshot.tuning.as_mut().unwrap().plan_scope = QueryStoreScope::Unknown;
    snapshot.tuning.as_mut().unwrap().recommendations = vec![index_recommendation("CreateIndex")];
    let query = &mut snapshot.workload.as_mut().unwrap().queries[0];
    query.text = "SELECT total FROM shop.orders WHERE channel = @channel AND created_at = @at".into();
    query.plan = None;
    let unpartitioned = findings(&snapshot);
    let positive = unpartitioned.iter().find(|finding| finding.id.starts_with("tuning-advice:create-index:")).unwrap();
    assert!(!positive.queries.is_empty(), "known unpartitioned runtime remains useful without plan-forcing scope");
    select(&mut snapshot, "7");
    let selected = findings(&snapshot);
    let native = selected.iter().find(|finding| finding.id == positive.id).unwrap();
    assert!(native.queries.is_empty());
    assert!(native.impact.db_time_share.is_none());
    assert!(!native.inactive);
    assert!(native.metrics.iter().any(|metric| metric.value == 64.0), "independent native score is preserved");
}

#[test]
fn native_drop_recommendation_cannot_bypass_all_read_member_usage_coverage() {
    use culprit_core::operations::IndexUsageScope;
    let mut snapshot = snapshot();
    snapshot.tuning.as_mut().unwrap().recommendations = vec![index_recommendation("DropIndex")];
    // Positive local use avoids the ordinary unused-index route; this is specifically the native recommendation.
    snapshot.indexes.iter_mut().find(|index| index.name == "ix_orders_channel").unwrap().reads = Some(5.0);
    for scope in [
        None,
        Some(IndexUsageScope::ConnectedMember),
        Some(IndexUsageScope::ObservedMembers),
        Some(IndexUsageScope::AllReadMembers),
    ] {
        snapshot.operations.index_usage_scope = scope;
        let findings = findings(&snapshot);
        let native = findings
            .iter()
            .find(|finding| finding.id == "tuning-advice:drop-index:shop.orders.ix_orders_channel")
            .unwrap();
        let complete = scope == Some(IndexUsageScope::AllReadMembers);
        assert_eq!(native.is_actionable(), complete);
        assert_eq!(native.fix.as_ref().unwrap().sql.is_some(), complete);
        assert!(native.metrics.iter().any(|metric| metric.value == 5.0));
        if !complete {
            assert_eq!(native.actionability, culprit_core::Actionability::Information);
            assert!(!native.verify.iter().any(|line| line.contains("after disabling")));
        }
    }
}

#[test]
fn observed_window_group_keeps_positive_runtime_but_cannot_supply_plan_management_authority() {
    let mut snapshot = force_snapshot();
    select(&mut snapshot, "7");
    snapshot.tuning.as_mut().unwrap().plan_scope = QueryStoreScope::ReplicaGroup { id: "7".into() };
    assert!(can_change_plan(&snapshot), "native-catalog control owns this exact management scope");
    let workload = snapshot.workload.as_mut().unwrap();
    workload.population_catalog.as_mut().unwrap().membership =
        culprit_core::RuntimePopulationMembership::ObservedWindow {
            from: workload.window_start.clone().unwrap(),
            until: workload.window_end.clone().unwrap(),
        };
    assert!(plan_workload(&snapshot).is_some(), "same-group positive runtime is retained");
    assert!(!can_change_plan(&snapshot));
    assert!(plan_command(&snapshot, PlanCommand::Unforce, "9", "9").is_none());
    let scan = findings(&snapshot).into_iter().find(|finding| finding.id == "tuning-advice:forced-scan:9").unwrap();
    assert!(scan.impact.db_time_share.is_some());
    assert!(scan.fix.as_ref().unwrap().sql.is_none());
    assert!(!scan.verify.iter().any(|line| line.contains("EXEC ")));
}
