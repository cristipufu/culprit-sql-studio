use super::*;
use culprit_core::DatabaseIdentity;
fn view() -> MssqlIndexSnapshot {
    MssqlIndexSnapshot {
        parent_kind: MssqlIndexParentKind::IndexedView,
        parent_object: MssqlObjectIdentity {
            database_id: 7,
            object_id: 101,
            created_without_offset: "2026-11-10T07:00:00.123".into(),
        },
        parent_schema: "reporting".into(),
        parent_name: "Revenue".into(),
        parent_modified_without_offset: "2026-11-10T07:01:00.123".into(),
        index_id: 1,
        name: "cx_revenue".into(),
        type_code: 1,
        native_type: "CLUSTERED".into(),
        parent_memory_optimized: None,
        unique: true,
        primary_key: false,
        disabled: false,
        columns: vec![],
        columnstore: None,
        configuration: None,
        filter: None,
        reserved_bytes: Some("9007199254740993".into()),
        usage: MssqlIndexUsage::Observed(Box::new(MssqlIndexUsageCounters {
            row_present: true,
            user_seeks: Some("9007199254740993".into()),
            user_scans: Some("0".into()),
            user_lookups: Some("0".into()),
            user_updates: Some("3".into()),
            last_user_seek_without_offset: Some("2026-11-10T07:00:10.123".into()),
            last_user_scan_without_offset: None,
            last_user_lookup_without_offset: None,
            last_user_update_without_offset: None,
            reset: culprit_core::ResetMarker::Unknown,
        })),
    }
}
fn outcome(state: SourceState) -> SourceOutcome {
    SourceOutcome { source: source_id::INDEXES.into(), state, reason: None, reason_message: None }
}
fn original() -> (CollectionDescription, OperationalObservation) {
    let index = view();
    let row = OperationalObservation {
        source: source_id::INDEXES.into(),
        key: index.key().unwrap(),
        fact: OperationalFact::MssqlIndex(Box::new(index)),
    };
    let description = CollectionDescription {
        target: DatabaseIdentity::server(Engine::Mssql, "original-db", "original.invalid", 1433, None).into(),
        source: "index-state".into(),
        partition: SourcePartition::Target,
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Indexes,
            observed_from: "2026-11-11T12:00:00Z".into(),
            observed_until: "2026-11-11T12:00:01Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::collected(source_id::INDEXES.into(), 1)],
        }),
    };
    (description, row)
}
fn current() -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.sources.retain(|source| source.source != source_id::INDEXES);
    snapshot.sources.push(outcome(SourceState::Collected));
    snapshot.operations.indexed_views = Some(Box::new(MssqlIndexedViewCatalog {
        source: outcome(SourceState::Collected),
        retained_index_rows: Some(3),
        parent_prefix_complete: true,
        indexes: vec![view()],
    }));
    snapshot
}
#[test]
fn current_and_original_keep_view_identity_usage_unknown_reset_and_local_clock_without_table_advice() {
    let snapshot = current();
    let current = explain_snapshot(&snapshot).unwrap().unwrap();
    assert_eq!(current.native_index_rows, Some(3));
    assert_eq!(current.observed_view_indexes, 1);
    assert!(current.context.is_none());
    let (source, row) = original();
    let report = explain(MssqlCatalogEvidence {
        description: &source,
        observations: &[&row],
        source: &outcome(SourceState::Collected),
        read_complete: true,
    })
    .unwrap();
    assert_eq!(report.context.unwrap().target, source.target);
    assert_eq!(report.native_index_rows, Some(1));
    let entry = &report.entries[0];
    assert_eq!(entry.parent_object.object_id, 101);
    assert_eq!(entry.reserved_bytes.as_deref(), Some("9007199254740993"));
    let MssqlIndexUsage::Observed(usage) = &entry.usage else { panic!() };
    assert_eq!(usage.user_seeks.as_deref(), Some("9007199254740993"));
    assert_eq!(usage.reset, culprit_core::ResetMarker::Unknown);
    assert_eq!(usage.last_user_seek_without_offset.as_deref(), Some("2026-11-10T07:00:10.123"));
    assert!(report.limitations.contains(&t!("view.indexed_views.limit.usage")));
}
#[test]
fn current_table_candidate_admission_is_exact_schema_and_never_uses_saved_originals_or_an_unknown_subject() {
    let mut snapshot = current();
    let table = snapshot.tables[0].clone();
    snapshot.tables = vec![table];
    snapshot.tables[0].schema = "dbo".into();
    snapshot.tables[0].name = "Revenue".into();
    assert!(crate::indexes::permits_table_index_advice(&snapshot, "dbo", "Revenue"));
    assert!(!crate::indexes::permits_table_index_advice(&snapshot, "reporting", "Revenue"));
    assert!(!crate::indexes::permits_table_index_advice(&snapshot, "missing", "Revenue"));
    snapshot.tables[0].schema = "reporting".into();
    assert!(
        !crate::indexes::permits_table_index_advice(&snapshot, "reporting", "Revenue"),
        "a contradictory current table label cannot override the native view"
    );
    snapshot.tables[0].schema = "dbo".into();
    snapshot.operations.indexed_views = None;
    snapshot.operations.indexed_view_originals.push(RecordedOperationalRead::Unavailable {
        reason: culprit_core::message!("view.indexed_views.sharing_withheld"),
    });
    assert!(crate::indexes::permits_table_index_advice(&snapshot, "dbo", "Revenue"));
    let duplicate = snapshot.tables[0].clone();
    snapshot.tables.push(duplicate);
    assert!(!crate::indexes::permits_table_index_advice(&snapshot, "dbo", "Revenue"));
    snapshot.server.engine = Engine::Postgres;
    assert!(
        crate::indexes::permits_table_index_advice(&snapshot, "missing", "Revenue"),
        "other-provider policy is unchanged"
    );
}
#[test]
fn partial_hidden_empty_and_unavailable_do_not_imply_a_healthy_global_view_inventory() {
    let mut snapshot = current();
    for state in [SourceState::Partial, SourceState::Unavailable] {
        snapshot.sources.last_mut().unwrap().state = state;
        let catalog = snapshot.operations.indexed_views.as_mut().unwrap();
        catalog.source.state = state;
        catalog.parent_prefix_complete = false;
        let report = explain_snapshot(&snapshot).unwrap().unwrap();
        assert_eq!(report.source.state, state);
        if state == SourceState::Unavailable {
            assert!(report.entries.is_empty());
            assert_eq!(report.summary, t!("view.indexed_views.unavailable"));
        } else {
            assert_eq!(report.entries.len(), 1);
            assert!(report.limitations.contains(&t!("view.indexed_views.partial")));
        }
        assert!(report.limitations.contains(&t!("view.indexed_views.limit.visibility")));
    }
    let catalog = snapshot.operations.indexed_views.as_mut().unwrap();
    catalog.indexes.clear();
    catalog.retained_index_rows = Some(0);
    catalog.parent_prefix_complete = true;
    catalog.source = outcome(SourceState::Empty);
    *snapshot.sources.last_mut().unwrap() = outcome(SourceState::Empty);
    let empty = explain_snapshot(&snapshot).unwrap().unwrap();
    assert!(empty.entries.is_empty());
    assert_eq!(empty.native_index_rows, Some(0));
    snapshot.sources.last_mut().unwrap().state = SourceState::Unavailable;
    assert!(
        explain_snapshot(&snapshot).is_err(),
        "derived/current source cannot promote an unavailable original catalog"
    );
}
#[test]
fn original_mixed_population_refuses_duplicate_foreign_generation_key_and_wrong_dataset_before_rendering() {
    let (source, row) = original();
    for cause in ["duplicate", "generation", "kind", "engine", "dataset", "source"] {
        let mut changed_source = source.clone();
        let mut changed = row.clone();
        match cause {
            "duplicate" => {
                let SourceObservation::Snapshot(header) = &mut changed_source.observation else { panic!() };
                header.records = 2;
                header.sources[0].records = 2;
            }
            "generation" => {
                let OperationalFact::MssqlIndex(index) = &mut changed.fact else { panic!() };
                index.parent_object.created_without_offset = "2026-11-10T07:00:00.124".into();
            }
            "kind" => {
                let OperationalFact::MssqlIndex(index) = &mut changed.fact else { panic!() };
                index.parent_kind = MssqlIndexParentKind::Table;
            }
            "engine" => {
                let CollectionTarget::Database(db) = &mut changed_source.target else { panic!() };
                db.engine = Engine::Postgres;
            }
            "dataset" => {
                let SourceObservation::Snapshot(header) = &mut changed_source.observation else { panic!() };
                header.dataset = SnapshotDataset::Tables;
            }
            _ => changed.source = source_id::TABLES.into(),
        }
        let rows = if cause == "duplicate" { vec![&changed, &changed] } else { vec![&changed] };
        assert!(
            explain(MssqlCatalogEvidence {
                description: &changed_source,
                observations: &rows,
                source: &outcome(SourceState::Collected),
                read_complete: true
            })
            .is_err(),
            "{cause}"
        );
    }
}

#[test]
fn missing_index_candidates_share_current_parent_admission_with_the_advisor() {
    let mut snapshot = current();
    let mut request = snapshot.missing_indexes.first().expect("native fixture request").clone();
    request.schema = "reporting".into();
    request.table = "Revenue".into();
    request.seeks = 100.0;
    request.avg_impact_pct = 90.0;
    snapshot.missing_indexes = vec![request];
    assert!(crate::indexes::merge_candidates(&snapshot).is_empty());
    let mut table = snapshot.tables[0].clone();
    table.schema = "dbo".into();
    table.name = "Revenue".into();
    snapshot.tables = vec![table];
    snapshot.missing_indexes[0].schema = "dbo".into();
    assert_eq!(crate::indexes::merge_candidates(&snapshot).len(), 1);
    snapshot.missing_indexes[0].schema = "missing".into();
    assert!(crate::indexes::merge_candidates(&snapshot).is_empty());
    // A retained original is not current catalog admission, even if its names match a removed subject.
    snapshot.operations.indexed_view_originals.push(RecordedOperationalRead::Unavailable {
        reason: culprit_core::message!("view.indexed_views.sharing_withheld"),
    });
    assert!(crate::indexes::merge_candidates(&snapshot).is_empty());
}

#[test]
fn filtered_scan_and_advisor_groups_refuse_current_views_unknown_subjects_and_same_leaf_schema_borrowing() {
    let mut snapshot = current();
    let mut plan = snapshot.queries().into_iter().find_map(|query| query.plan.clone()).expect("plan fixture");
    plan.root.children.clear();
    plan.root.facts = Default::default();
    plan.root.props.clear();
    plan.root.kind = culprit_core::NodeKind::Scan;
    plan.root.op = "Clustered Index Scan".into();
    plan.root.object = Some("reporting.Revenue".into());
    plan.root.predicate = Some("customer_id = 42".into());
    plan.root.rows_read = Some(100_000.0);
    plan.root.table_rows = Some(100_000.0);
    plan.root.act_rows = Some(10.0);
    plan.root.est_rows = 10.0;
    plan.root.act_executions = Some(1.0);
    plan.root.est_executions = 1.0;
    plan.kind = culprit_core::PlanKind::Actual;
    plan.index_hints.clear();
    plan.raw = None;
    plan.insights = None;
    let mut query = snapshot.queries().into_iter().next().unwrap().clone();
    query.text = "SELECT id FROM reporting.Revenue WHERE customer_id = 42".into();
    query.plan = Some(plan);
    query.total_time_ms = 1000.0;
    query.executions = 100.0;
    let workload = snapshot.workload.as_mut().unwrap();
    workload.queries = vec![query];
    workload.total_time_ms = 1000.0;
    let mut table = snapshot.tables[0].clone();
    table.schema = "dbo".into();
    table.name = "Revenue".into();
    snapshot.tables = vec![table];
    assert!(crate::indexes::scan_groups(&snapshot).is_empty());
    snapshot.workload.as_mut().unwrap().queries[0].plan.as_mut().unwrap().root.object = Some("dbo.Revenue".into());
    assert_eq!(crate::indexes::scan_groups(&snapshot).len(), 1, "known table control is independently meaningful");
    {
        let original = snapshot.workload.as_mut().unwrap().queries[0].plan.as_mut().unwrap();
        original.root.object = Some("Revenue".into());
        original.root.facts_mut().mssql_index_parent_kind = Some(MssqlIndexParentKind::IndexedView);
    }
    assert!(
        crate::indexes::scan_groups(&snapshot).is_empty(),
        "original native view kind cannot borrow the dbo table through the legacy leaf fallback"
    );
    snapshot.tables.clear();
    assert!(
        crate::indexes::scan_groups(&snapshot).is_empty(),
        "a saved/historical plan cannot establish a current table subject"
    );
}
