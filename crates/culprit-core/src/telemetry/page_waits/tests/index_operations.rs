use super::*;

fn with_point() -> PageWaitObservation {
    let mut row = observation();
    let target = row.target.as_mut().unwrap();
    target.header.partition_id = Some("9007199254740993".into());
    assert!(
        target
            .object
            .as_mut()
            .unwrap()
            .native
            .qualifiers
            .insert("native-file".into(), "point-private-canary".into())
            .is_none()
    );
    let counters = MssqlIndexOperations {
        identity: MssqlIndexIdentity {
            database_id: 5,
            object_id: 128,
            index_id: 1,
            partition_number: 1,
            hobt_id: None,
        },
        schema: Some("dbo".into()),
        object: Some("orders".into()),
        index: Some("orders_id".into()),
        object_type: Some("U".into()),
        engine_internal: Some(false),
        leading_key_is_identity: None,
        leading_key_default: None,
        optimize_for_sequential_key: None,
        counters: [
            (MssqlIndexCounter::PageLatchWaits, Some("3".into())),
            (MssqlIndexCounter::PageLatchWaitMs, Some("9007199254740993".into())),
            (MssqlIndexCounter::PageIoLatchWaits, None),
            (MssqlIndexCounter::PageIoLatchWaitMs, Some("0".into())),
        ]
        .into(),
        reset: crate::ResetMarker::Unknown,
    };
    row.index_operations = Some(PageIndexOperations::Observed {
        observed_at: "2026-11-13T10:00:00.5Z".into(),
        counters: Box::new(counters),
        partition_id: target.header.partition_id.clone(),
        object: target.object.as_ref().map(|object| object.native.clone()),
        relation: PageIndexOperationRelation::CheckedTarget,
    });
    assert!(row.is_valid());
    row
}

#[test]
fn independent_index_points_keep_exact_values_unknown_reset_and_ended_or_changed_requests() {
    let original = with_point();
    for state in [PageWaitRequestState::SameStatement, PageWaitRequestState::Changed, PageWaitRequestState::Missing] {
        let mut row = original.clone();
        let check = row.after_check.as_mut().unwrap();
        check.request = state;
        check.wait = if state == PageWaitRequestState::SameStatement {
            PageWaitRelation::NoLongerObserved
        } else {
            row.statement_identity = None;
            row.plan_identity = None;
            PageWaitRelation::Unqualified
        };
        assert!(row.is_valid());
        assert_eq!(row.index_operations, original.index_operations);
        let roundtrip: PageWaitObservation = serde_json::from_str(&serde_json::to_string(&row).unwrap()).unwrap();
        assert_eq!(roundtrip, row);
    }
    let PageIndexOperations::Observed { counters, .. } = original.index_operations.unwrap() else { panic!() };
    assert_eq!(counters.counter(MssqlIndexCounter::PageIoLatchWaitMs), Some("0"));
    assert_eq!(counters.counter(MssqlIndexCounter::PageLatchWaitMs), Some("9007199254740993"));
    assert_eq!(counters.counters[&MssqlIndexCounter::PageIoLatchWaits], None);
    assert_eq!(counters.reset, crate::ResetMarker::Unknown);
}

#[test]
fn changed_catalog_keeps_a_counter_point_without_claiming_a_link_to_the_original_page() {
    let mut row = with_point();
    row.target = None;
    row.resolution = PageWaitResolution::ChangedMetadata;
    assert!(!row.is_valid(), "the checked link needs its target");
    let Some(PageIndexOperations::Observed { relation, .. }) = &mut row.index_operations else { panic!() };
    *relation = PageIndexOperationRelation::ChangedTarget;
    assert!(row.is_valid());
    let Some(PageIndexOperations::Observed { partition_id, object, relation, .. }) = &mut row.index_operations else {
        panic!()
    };
    *partition_id = None;
    *object = None;
    *relation = PageIndexOperationRelation::UnverifiedTarget;
    assert!(row.is_valid(), "missing catalog qualification preserves the independent native point");
}

#[test]
fn a_point_cannot_borrow_another_database_partition_catalog_or_clock() {
    for field in ["database", "partition", "object", "counter", "clock", "partition-number", "unbounded-id"] {
        let mut row = with_point();
        let Some(PageIndexOperations::Observed { observed_at, counters, partition_id, object, .. }) =
            &mut row.index_operations
        else {
            panic!()
        };
        match field {
            "database" => counters.identity.database_id = 6,
            "partition" => *partition_id = Some("9007199254740994".into()),
            "object" => {
                assert!(
                    object.as_mut().unwrap().qualifiers.insert("native-file".into(), "another-file".into()).is_some()
                );
            }
            "counter" => {
                assert!(counters.counters.insert(MssqlIndexCounter::PageLatchWaits, Some("-1".into())).is_some());
            }
            "clock" => *observed_at = "2026-11-13T10:00:01Z".into(),
            "partition-number" => counters.identity.partition_number = 0,
            "unbounded-id" => *partition_id = Some(format!("{}1", "0".repeat(100))),
            _ => unreachable!(),
        }
        assert!(!row.is_valid(), "{field}");
    }
    let mut row = with_point();
    row.request.scope = ActivityScopeBasis::OtherDatabase;
    row.target = None;
    row.resolution = PageWaitResolution::OutsideDatabase;
    let Some(PageIndexOperations::Observed { relation, .. }) = &mut row.index_operations else { panic!() };
    *relation = PageIndexOperationRelation::UnverifiedTarget;
    assert!(!row.is_valid(), "unlinked counters still need their observed database context");
}

#[test]
fn sharing_strips_private_catalog_qualifiers_for_linked_and_unlinked_points_without_mutating_capture() {
    for linked in [true, false] {
        let mut original: crate::Snapshot =
            serde_json::from_str(include_str!("../../../../../../fixtures/mssql-demo.json")).unwrap();
        let mut row = with_point();
        if !linked {
            row.target = None;
            row.resolution = PageWaitResolution::ChangedMetadata;
            let Some(PageIndexOperations::Observed { relation, .. }) = &mut row.index_operations else { panic!() };
            *relation = PageIndexOperationRelation::ChangedTarget;
        }
        let mut evidence = snapshot();
        evidence.observations = vec![row];
        original.operations.page_waits = Some(evidence.clone());
        let shared = crate::redact::redact_snapshot(&original);
        assert!(!serde_json::to_string(&shared).unwrap().contains("point-private-canary"));
        let shared = shared.operations.page_waits.as_ref().unwrap();
        assert!(shared.is_valid(), "linked={linked}");
        let Some(PageIndexOperations::Observed { counters, object, .. }) = &shared.observations[0].index_operations
        else {
            panic!()
        };
        assert_eq!(counters.identity.object_id, 128);
        assert_eq!(object.as_ref().unwrap().id, "128");
        assert_eq!(counters.counter(MssqlIndexCounter::PageLatchWaitMs), Some("9007199254740993"));
        assert_eq!(original.operations.page_waits.as_ref().unwrap(), &evidence);
        assert!(serde_json::to_string(&evidence).unwrap().contains("point-private-canary"));
    }
}
