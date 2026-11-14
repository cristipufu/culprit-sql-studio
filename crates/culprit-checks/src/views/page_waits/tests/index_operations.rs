use super::*;

fn with_point(kind: WaitResourceKind) -> PageWaitObservation {
    let mut row = observation("61", kind);
    let target = row.target.as_ref().unwrap();
    row.index_operations = Some(PageIndexOperations::Observed {
        observed_at: "2026-11-13T10:00:00.5Z".into(),
        partition_id: target.header.partition_id.clone(),
        object: target.object.as_ref().map(|object| object.native.clone()),
        relation: PageIndexOperationRelation::CheckedTarget,
        counters: Box::new(MssqlIndexOperations {
            identity: MssqlIndexIdentity {
                database_id: 7,
                object_id: 128,
                index_id: 1,
                partition_number: 1,
                hobt_id: None,
            },
            schema: Some("app".into()),
            object: Some("orders".into()),
            index: Some("ix_orders".into()),
            object_type: Some("U".into()),
            engine_internal: Some(false),
            leading_key_is_identity: None,
            leading_key_default: None,
            optimize_for_sequential_key: None,
            counters: [
                (MssqlIndexCounter::PageLatchWaits, Some("3".into())),
                (MssqlIndexCounter::PageLatchWaitMs, Some("9007199254740993".into())),
                (MssqlIndexCounter::PageIoLatchWaits, Some("1".into())),
                (MssqlIndexCounter::PageIoLatchWaitMs, Some("12".into())),
            ]
            .into(),
            reset: culprit_core::ResetMarker::Unknown,
        }),
    });
    assert!(row.is_valid());
    row
}

#[test]
fn separate_native_memory_and_io_counters_keep_exact_totals_without_an_elapsed_share_or_owner() {
    for (kind, message, waits, milliseconds) in [
        (WaitResourceKind::MemoryPage, "view.page_waits.index.memory", "3", "9007199254740993"),
        (WaitResourceKind::PageIo, "view.page_waits.index.io", "1", "12"),
    ] {
        let row = with_point(kind);
        let report = explain(&[&row], Some(&source(SourceState::Partial))).unwrap();
        let expected =
            culprit_core::i18n::t(message, &[("waits", waits.into()), ("milliseconds", milliseconds.into())]);
        assert!(report.entries[0].explanation.contains(&expected));
        assert!(report.limitations.contains(&t!("view.page_waits.limit.index_counters")));
        assert_eq!(row.request.wait.as_ref().unwrap().elapsed_ms.as_deref(), Some("1"));
        assert!(!report.entries[0].explanation.contains('%'));
        assert_eq!(report.entries[0].request_key, row.request.key);
    }
}

#[test]
fn unknown_counter_components_are_never_inserted_as_measured_zero() {
    let mut row = with_point(WaitResourceKind::MemoryPage);
    let Some(PageIndexOperations::Observed { counters, .. }) = &mut row.index_operations else { panic!() };
    assert!(counters.counters.insert(MssqlIndexCounter::PageLatchWaits, None).is_some());
    let report = explain(&[&row], None).unwrap();
    assert!(report.entries[0].explanation.contains(&t!("view.page_waits.index.memory_unknown")));
    let Some(PageIndexOperations::Observed { counters, .. }) = &mut row.index_operations else { panic!() };
    assert!(counters.counters.insert(MssqlIndexCounter::PageLatchWaits, Some("0".into())).is_some());
    assert!(counters.counters.insert(MssqlIndexCounter::PageLatchWaitMs, Some("0".into())).is_some());
    let report = explain(&[&row], None).unwrap();
    assert!(report.entries[0].explanation.contains(&t!(
        "view.page_waits.index.memory",
        waits = "0",
        milliseconds = "0"
    )));
}

#[test]
fn changed_or_unqualified_mapping_keeps_positive_points_with_an_explicit_unlinked_explanation() {
    for (relation, message) in [
        (PageIndexOperationRelation::ChangedTarget, "view.page_waits.index.changed"),
        (PageIndexOperationRelation::UnverifiedTarget, "view.page_waits.index.unverified"),
    ] {
        let mut row = with_point(WaitResourceKind::MemoryPage);
        row.target = None;
        row.resolution = PageWaitResolution::ChangedMetadata;
        let Some(PageIndexOperations::Observed { relation: native_relation, .. }) = &mut row.index_operations else {
            panic!()
        };
        *native_relation = relation;
        let report = explain(&[&row], None).unwrap();
        assert!(report.entries[0].explanation.contains("9007199254740993"));
        assert!(report.entries[0].explanation.contains(&culprit_core::i18n::t(message, &[])));
        assert!(!report.entries[0].explanation.contains("app.orders"));
    }
}

#[test]
fn missing_or_denied_points_have_explicit_unknowns_without_erasing_current_page_metadata() {
    for (point, message) in [
        (PageIndexOperations::Missing, "view.page_waits.index.missing"),
        (PageIndexOperations::PermissionDenied, "view.page_waits.index.denied"),
        (PageIndexOperations::Unavailable, "view.page_waits.index.unavailable"),
    ] {
        let mut row = observation("61", WaitResourceKind::MemoryPage);
        row.index_operations = Some(point);
        let report = explain(&[&row], None).unwrap();
        assert!(report.entries[0].explanation.contains(&culprit_core::i18n::t(message, &[])));
        assert!(report.entries[0].explanation.contains("app.orders"));
    }
}
