use super::*;

fn end_of_leaf() -> (PageWaitWriterFocus, NativeReadContext, PageWaitWriterObservation) {
    let (focus, context, mut row) = fixture();
    let mut point = super::super::index_operations::with_point(WaitResourceKind::MemoryPage).index_operations.unwrap();
    let page = row.page.as_mut().unwrap();
    let target = page.target.as_ref().unwrap();
    let PageIndexOperations::Observed { object, counters, .. } = &mut point else { unreachable!() };
    *object = target.object.as_ref().map(|object| object.native.clone());
    counters.leading_key_is_identity = Some(true);
    counters.optimize_for_sequential_key = Some(false);
    page.index_operations = Some(point);
    assert!(focus.accepts_observation(&context, &row));
    (focus, context, row)
}

fn report(
    focus: &PageWaitWriterFocus,
    context: &NativeReadContext,
    row: &PageWaitWriterObservation,
) -> culprit_core::PageWaitWriterReport {
    assert!(focus.accepts_observation(context, row), "negative controls must remain valid native evidence");
    explain_writers(focus, context, Some(row), Some(&source(SourceState::Partial))).unwrap()
}

fn has_candidate(report: &culprit_core::PageWaitWriterReport) -> bool {
    report.entries.iter().any(|entry| entry.explanation.contains(&t!("view.page_writers.insert_hotspot.candidate")))
}

fn rebind_target(focus: &mut PageWaitWriterFocus, row: &mut PageWaitWriterObservation) {
    let page = row.page.as_mut().unwrap();
    let target = page.target.as_ref().unwrap();
    let Some(PageIndexOperations::Observed { object, counters, .. }) = &mut page.index_operations else {
        unreachable!()
    };
    *object = target.object.as_ref().map(|object| object.native.clone());
    counters.identity.index_id = target.header.index_id.as_ref().unwrap().parse().unwrap();
    focus.target = target.clone();
    for candidate in &mut row.candidates {
        if let Some(binding) = &candidate.binding {
            let accesses = binding.accesses.clone();
            candidate.binding = Some(
                MssqlPageWritePlanProof::new(
                    &focus.target,
                    candidate,
                    digest(b"native-plan"),
                    digest(b"native-sql"),
                    accesses,
                )
                .unwrap(),
            );
        }
    }
    row.focus_key = focus.key().unwrap();
}

#[test]
fn recorded_end_leaf_and_bound_insert_plan_explain_a_candidate_with_query_context_and_bounded_verification() {
    let (focus, context, row) = end_of_leaf();
    let original = row.clone();
    let report = report(&focus, &context, &row);
    let entry = &report.entries[0];
    assert!(has_candidate(&report));
    assert_eq!(entry.request, focus.candidates[0].key);
    assert_eq!(entry.parent, focus.candidates[0].parent);
    assert_eq!(entry.accesses, [MssqlPageWriteAccess { node_id: 1, operation: PageWriteOperation::Insert }]);
    assert!(entry.explanation.starts_with(&t!("view.page_writers.relation.target")));
    assert!(entry.explanation.contains(&t!("view.page_writers.insert_hotspot.identity")));
    assert!(entry.explanation.contains(&t!("view.page_writers.insert_hotspot.pacing_off")));
    assert_eq!(entry.investigation, t!("view.page_writers.insert_hotspot.investigate"));
    assert_eq!(entry.verify, t!("view.page_writers.insert_hotspot.verify"));
    assert_eq!(report.original_target, focus.target);
    assert!(report.limitations.contains(&t!("view.page_writers.limit.execution")));
    assert!(report.limitations.contains(&t!("view.page_writers.limit.ownership")));
    assert!(report.limitations.contains(&t!("view.page_writers.limit.partial")));
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(encoded.contains("INSERT INTO app.orders"));
    for private in ["private-writer-value", "<ShowPlanXML", "\"binding\""] {
        assert!(!encoded.contains(private));
    }
    assert_eq!(row, original, "presentation never rewrites the original native proof");
}

#[test]
fn next_links_are_observed_position_only_and_unknown_or_nonterminal_links_keep_generic_advice() {
    for (file, page) in
        [(None, None), (Some(0), None), (None, Some(0)), (Some(1), Some(0)), (Some(0), Some(12)), (Some(1), Some(12))]
    {
        let (focus, context, mut row) = end_of_leaf();
        let header = &mut row.page.as_mut().unwrap().target.as_mut().unwrap().header;
        header.next_file_id = file;
        header.next_page_id = page;
        let report = report(&focus, &context, &row);
        assert!(!has_candidate(&report));
        assert_eq!(report.entries[0].explanation, t!("view.page_writers.relation.target"));
        assert_eq!(report.entries[0].investigation, t!("view.page_writers.investigate.target"));
    }
}

#[test]
fn source_visibility_and_checked_index_incarnation_are_required_without_treating_totals_as_a_rate() {
    let (focus, context, row) = end_of_leaf();
    let unknown_source = explain_writers(&focus, &context, Some(&row), None).unwrap();
    assert!(!has_candidate(&unknown_source));
    for point in [
        None,
        Some(PageIndexOperations::Missing),
        Some(PageIndexOperations::PermissionDenied),
        Some(PageIndexOperations::Unavailable),
    ] {
        let mut changed = row.clone();
        changed.page.as_mut().unwrap().index_operations = point;
        assert!(!has_candidate(&report(&focus, &context, &changed)));
    }
    for relation in [PageIndexOperationRelation::ChangedTarget, PageIndexOperationRelation::UnverifiedTarget] {
        let mut changed = row.clone();
        let Some(PageIndexOperations::Observed { relation: actual, .. }) =
            &mut changed.page.as_mut().unwrap().index_operations
        else {
            unreachable!()
        };
        *actual = relation;
        assert!(!has_candidate(&report(&focus, &context, &changed)));
    }
    let mut no_totals = row.clone();
    let Some(PageIndexOperations::Observed { counters, .. }) = &mut no_totals.page.as_mut().unwrap().index_operations
    else {
        unreachable!()
    };
    counters.counters.clear();
    let report = report(&focus, &context, &no_totals);
    assert!(has_candidate(&report), "the actual page wait remains positive without extrapolating native totals");
    assert!(!report.entries[0].explanation.contains('%'));
}

#[test]
fn identity_and_supported_pacing_are_context_only_and_do_not_establish_insert_progression() {
    for identity in [None, Some(false), Some(true)] {
        for pacing in [None, Some(false), Some(true)] {
            let (focus, context, mut row) = end_of_leaf();
            let Some(PageIndexOperations::Observed { counters, .. }) = &mut row.page.as_mut().unwrap().index_operations
            else {
                unreachable!()
            };
            counters.leading_key_is_identity = identity;
            counters.optimize_for_sequential_key = pacing;
            let report = report(&focus, &context, &row);
            assert!(has_candidate(&report));
            let explanation = &report.entries[0].explanation;
            assert_eq!(explanation.contains(&t!("view.page_writers.insert_hotspot.identity")), identity == Some(true));
            assert_eq!(explanation.contains(&t!("view.page_writers.insert_hotspot.pacing_on")), pacing == Some(true));
            assert_eq!(explanation.contains(&t!("view.page_writers.insert_hotspot.pacing_off")), pacing == Some(false));
            assert_eq!(report.entries[0].relation, PageWriterRelation::TargetObjectWrite);
        }
    }
}

#[test]
fn updates_readers_other_targets_unknown_commands_and_refused_generations_never_inherit_the_insert_candidate() {
    for operation in [PageWriteOperation::Update, PageWriteOperation::Delete] {
        let (focus, context, mut row) = end_of_leaf();
        let candidate = &mut row.candidates[0];
        candidate.request.as_mut().unwrap().command =
            Some(if operation == PageWriteOperation::Update { "UPDATE" } else { "DELETE" }.into());
        candidate.binding = Some(
            MssqlPageWritePlanProof::new(
                &focus.target,
                candidate,
                digest(b"native-plan"),
                digest(b"native-sql"),
                vec![MssqlPageWriteAccess { node_id: 1, operation }],
            )
            .unwrap(),
        );
        assert!(!has_candidate(&report(&focus, &context, &row)));
    }
    for relation in [
        PageWriterRelation::ReadOnlyPlan,
        PageWriterRelation::OtherTarget,
        PageWriterRelation::Unavailable,
        PageWriterRelation::Unsupported,
    ] {
        let (focus, context, mut row) = end_of_leaf();
        row.candidates[0].relation = relation;
        row.candidates[0].binding = None;
        assert!(!has_candidate(&report(&focus, &context, &row)));
    }
    let (focus, context, mut row) = end_of_leaf();
    row.candidates[0].request.as_mut().unwrap().command = None;
    assert!(!has_candidate(&report(&focus, &context, &row)));
    let (focus, context, mut row) = end_of_leaf();
    withdraw_generation(&mut row.candidates[0]);
    assert!(!has_candidate(&report(&focus, &context, &row)));
}

#[test]
fn lost_current_mapping_never_promotes_historical_end_links_to_a_current_insert_candidate() {
    for state in [
        PageWriterTargetState::NoLongerObserved,
        PageWriterTargetState::ChangedTarget,
        PageWriterTargetState::Unavailable,
    ] {
        let (focus, context, mut row) = end_of_leaf();
        row.target_state = state;
        row.page = None;
        row.candidates[0].binding = None;
        row.candidates[0].relation = PageWriterRelation::Unavailable;
        let report = report(&focus, &context, &row);
        assert!(!has_candidate(&report));
        assert_eq!(report.original_target.header.next_file_id, Some(0));
        assert_eq!(report.original_target.header.next_page_id, Some(0));
    }
    let (focus, context, _) = end_of_leaf();
    let missing = explain_writers(&focus, &context, None, Some(&source(SourceState::Collected))).unwrap();
    assert!(!has_candidate(&missing));
    assert_eq!(missing.source_state, Some(SourceState::Partial));
}

#[test]
fn branch_heap_internal_and_unknown_metadata_do_not_acquire_last_leaf_insert_advice() {
    for heap in [false, true] {
        let (mut focus, context, mut row) = end_of_leaf();
        let target = row.page.as_mut().unwrap().target.as_mut().unwrap();
        if heap {
            target.kind = PageWaitTargetKind::HeapData;
            target.header.index_id = Some("0".into());
            let object = target.object.as_mut().unwrap();
            object.index_name = None;
            object.native.qualifiers.insert("index_type".into(), "0".into());
            object.native.qualifiers.insert("index_id".into(), "0".into());
        } else {
            target.kind = PageWaitTargetKind::IndexBranch;
            target.header.page_type = Some(2);
            target.header.level = Some(1);
        }
        rebind_target(&mut focus, &mut row);
        assert!(!has_candidate(&report(&focus, &context, &row)));
    }
    for (object_type, internal) in
        [(Some("U"), None), (None, Some(false)), (Some("IT"), Some(false)), (Some("U"), Some(true))]
    {
        let (focus, context, mut row) = end_of_leaf();
        let Some(PageIndexOperations::Observed { counters, .. }) = &mut row.page.as_mut().unwrap().index_operations
        else {
            unreachable!()
        };
        counters.object_type = object_type.map(str::to_owned);
        counters.engine_internal = internal;
        assert!(!has_candidate(&report(&focus, &context, &row)));
    }
}

#[test]
fn allocation_columnstore_unknown_and_io_targets_are_refused_by_the_existing_writer_route() {
    for index_type in ["5", "6"] {
        let (mut focus, context, row) = end_of_leaf();
        focus.target.object.as_mut().unwrap().native.qualifiers.insert("index_type".into(), index_type.into());
        assert!(explain_writers(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).is_err());
    }
    for (kind, native_type) in [
        (PageWaitTargetKind::Pfs, 11),
        (PageWaitTargetKind::Gam, 8),
        (PageWaitTargetKind::Sgam, 9),
        (PageWaitTargetKind::Iam, 10),
        (PageWaitTargetKind::Unclassified, 99),
    ] {
        let (mut focus, context, row) = end_of_leaf();
        focus.target.kind = kind;
        focus.target.header.page_type = Some(native_type);
        focus.target.header.index_id = Some("0".into());
        focus.target.object = None;
        assert!(focus.target.is_valid());
        assert!(explain_writers(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).is_err());
    }
    let (mut focus, context, row) = end_of_leaf();
    focus.waiter.resource.kind = WaitResourceKind::PageIo;
    let original = &mut focus.waiter.participant_mut().original;
    let wait = original.wait.as_mut().unwrap();
    wait.native_type = Some("PAGEIOLATCH_SH".into());
    wait.identified_resources[0].kind = WaitResourceKind::PageIo;
    assert!(explain_writers(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).is_err());
}

#[test]
fn missing_native_wait_mixed_write_operators_and_negative_session_peers_withhold_the_new_advice() {
    let (focus, context, mut row) = end_of_leaf();
    row.page.as_mut().unwrap().request.wait.as_mut().unwrap().native_type = None;
    assert!(!has_candidate(&report(&focus, &context, &row)));
    let (focus, context, mut row) = end_of_leaf();
    let candidate = &mut row.candidates[0];
    candidate.binding = Some(
        MssqlPageWritePlanProof::new(
            &focus.target,
            candidate,
            digest(b"native-plan"),
            digest(b"native-sql"),
            vec![
                MssqlPageWriteAccess { node_id: 1, operation: PageWriteOperation::Insert },
                MssqlPageWriteAccess { node_id: 2, operation: PageWriteOperation::Update },
            ],
        )
        .unwrap(),
    );
    assert!(!has_candidate(&report(&focus, &context, &row)));
    let (mut focus, context, mut row) = end_of_leaf();
    add_candidate(&mut focus, &mut row, "62", "1", "SELECT payload FROM app.orders", PageWriterRelation::ReadOnlyPlan);
    withdraw_generation(&mut row.candidates[1]);
    let result = report(&focus, &context, &row);
    assert!(!has_candidate(&result));
    assert_eq!(result.entries[0].request_state, PageWaitRequestState::Unverified);
    assert!(result.entries[0].sql.is_none());
}
