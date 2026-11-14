use super::*;
use culprit_core::{Engine, ObservedWaitResource, telemetry::*};
use serde_json::json;
mod index_operations;

pub(crate) fn observation(session: &str, kind: WaitResourceKind) -> PageWaitObservation {
    let resource = ObservedWaitResource {
        kind,
        native: NativeIdentity::new("7:1:4144").qualified("source", "native-request-wait-resource"),
    };
    let request=serde_json::from_value(json!({
        "key":{"session":{"id":session,"generation":"native-login-1"},"request":{"id":"0","generation":"native-request-1"}},
        "blocking_status":"known","idle":false,"scope":"request-database","state":"suspended","database":"shop",
        "open_transactions":0,"session_counters":{},"request_counters":{},
        "client":{"application":"private-app-label"},
        "wait":{"native_type":if kind==WaitResourceKind::MemoryPage {"PAGELATCH_EX"} else {"PAGEIOLATCH_SH"},
            "elapsed_ms":"1","owners":[{"kind":"unknown","native_id":"-5","reason":"untracked-latch"}],
            "identified_resources":[resource.clone()]},
        "statement":DefinitionPayload::sql("SELECT payload FROM app.orders WHERE customer=9264",Engine::Mssql)
    })).unwrap();
    let row = PageWaitObservation {
        source: "native page observations".into(),
        observed_from: "2026-11-13T10:00:00.1Z".into(),
        observed_until: "2026-11-13T10:00:00.9Z".into(),
        request,
        before_check: Some(PageWaitRecheck {
            observed_at: "2026-11-13T10:00:00.2Z".into(),
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        }),
        after_check: Some(PageWaitRecheck {
            observed_at: "2026-11-13T10:00:00.8Z".into(),
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        }),
        statement_identity: Some(NativeIdentity::new("native-handle").qualified("statement_start_offset", "0")),
        plan_identity: None,
        index_operations: None,
        resource,
        observed_address: Some(PageWaitAddress { database_id: 7, file_id: 1, page_id: 4144 }),
        resolution: PageWaitResolution::ObservedAtBothBounds,
        target: Some(Box::new(MappedPageTarget {
            identity_basis: PageTargetIdentityBasis::LocalDatabaseFileEpoch,
            database: NativeIdentity::new("7").qualified("instance_epoch", "native-epoch"),
            file: NativeIdentity::new("native-file-guid"),
            header: PageHeaderFacts {
                database_id: 7,
                file_id: 1,
                page_id: 4144,
                header_version: Some(1),
                page_type: Some(1),
                level: Some(0),
                object_id: Some("128".into()),
                index_id: Some("1".into()),
                partition_id: Some("9007199254740993".into()),
                allocation_unit_id: None,
                lsn: None,
                next_file_id: Some(0),
                next_page_id: Some(0),
            },
            kind: PageWaitTargetKind::IndexLeaf,
            object: Some(PageWaitObject {
                native: NativeIdentity::new("128"),
                schema: "app".into(),
                name: "orders".into(),
                index_name: Some("ix_orders".into()),
            }),
        })),
    };
    assert!(row.is_valid());
    row
}
fn source(state: SourceState) -> SourceOutcome {
    SourceOutcome { source: "native page observations".into(), state, reason: None, reason_message: None }
}

#[test]
fn same_page_waiting_requests_stay_distinct_and_never_acquire_latch_or_sql_ownership() {
    let first = observation("61", WaitResourceKind::MemoryPage);
    let second = observation("62", WaitResourceKind::MemoryPage);
    let report = explain(&[&first, &second], Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert_ne!(report.entries[0].observation_key, report.entries[1].observation_key);
    assert_eq!(report.entries[0].request_key, first.request.key);
    assert_eq!(report.entries[1].request_key, second.request.key);
    assert_eq!(report.entries[0].resource.native.id, "7:1:4144");
    assert!(report.limitations.contains(&t!("view.page_waits.limit.ownership")));
    assert!(report.limitations.contains(&t!("view.page_waits.limit.bounds")));
    assert!(report.entries[0].explanation.contains("app.orders"));
    let rendered = serde_json::to_string(&report).unwrap();
    for private in ["SELECT payload", "9264", "private-app-label", "9007199254740993"] {
        assert!(!rendered.contains(private));
    }
    assert!(
        first.target.as_ref().unwrap().header.partition_id.as_deref() == Some("9007199254740993"),
        "large raw facts remain owned by the source row"
    );
}

#[test]
fn io_waits_do_not_become_in_memory_latch_diagnoses_or_a_storage_cause_verdict() {
    let memory = observation("61", WaitResourceKind::MemoryPage);
    let io = observation("62", WaitResourceKind::PageIo);
    let report = explain(&[&memory, &io], None).unwrap();
    assert_eq!(report.entries[0].title, t!("view.page_waits.memory.title"));
    assert_eq!(report.entries[1].title, t!("view.page_waits.io.title"));
    assert!(report.entries[1].explanation.contains(&t!("view.page_waits.io.explanation")));
    assert_eq!(report.entries[1].investigation, t!("view.page_waits.investigation.io"));
    assert_eq!(report.entries[0].investigation, t!("view.page_waits.investigation.index"));
}

#[test]
fn typed_allocation_heap_and_unclassified_targets_permit_only_their_bounded_next_investigation() {
    for (kind, message, investigation) in [
        (PageWaitTargetKind::Allocation, "view.page_waits.kind.allocation", "view.page_waits.investigation.allocation"),
        (PageWaitTargetKind::HeapData, "view.page_waits.kind.heap", "view.page_waits.investigation.heap"),
        (
            PageWaitTargetKind::Unclassified,
            "view.page_waits.kind.unclassified",
            "view.page_waits.investigation.unclassified",
        ),
        (PageWaitTargetKind::IndexBranch, "view.page_waits.kind.branch", "view.page_waits.investigation.index"),
    ] {
        let mut row = observation("61", WaitResourceKind::MemoryPage);
        row.target.as_mut().unwrap().kind = kind;
        row.target.as_mut().unwrap().object = None;
        let report = explain(&[&row], None).unwrap();
        let entry = &report.entries[0];
        assert!(entry.explanation.contains(&culprit_core::i18n::t(message, &[])));
        assert_eq!(entry.investigation, culprit_core::i18n::t(investigation, &[]));
        assert!(!entry.investigation.contains("ALTER "));
        assert!(!entry.explanation.contains("app.orders"));
    }
}

#[test]
fn unresolved_scope_permissions_and_reuse_preserve_original_waiter_without_a_current_target() {
    for resolution in [
        PageWaitResolution::OutsideDatabase,
        PageWaitResolution::Unsupported,
        PageWaitResolution::PermissionDenied,
        PageWaitResolution::Unavailable,
        PageWaitResolution::ChangedMetadata,
        PageWaitResolution::UnverifiedSource,
        PageWaitResolution::UnrecognizedAddress,
        PageWaitResolution::Limit,
    ] {
        let mut row = observation("61", WaitResourceKind::MemoryPage);
        row.resolution = resolution;
        row.target = None;
        row.statement_identity = None;
        let report = explain(&[&row], Some(&source(SourceState::Partial))).unwrap();
        assert_eq!(report.entries[0].observation_key, row.key().unwrap());
        assert_eq!(report.entries[0].request_key, row.request.key);
        assert!(report.entries[0].explanation.contains(&culprit_core::i18n::t(unresolved_message(resolution), &[])));
        assert!(!report.entries[0].explanation.contains("app.orders"));
        assert_eq!(report.entries[0].investigation, t!("view.page_waits.investigation.unresolved"));
    }
}

#[test]
fn later_wait_completion_or_a_changed_request_never_rewrites_original_participant_or_current_header_evidence() {
    for (request, wait, key) in [
        (
            PageWaitRequestState::SameStatement,
            PageWaitRelation::NoLongerObserved,
            "view.page_waits.recheck.no_longer_observed",
        ),
        (PageWaitRequestState::SameStatement, PageWaitRelation::Different, "view.page_waits.recheck.different"),
        (PageWaitRequestState::Missing, PageWaitRelation::Unqualified, "view.page_waits.recheck.missing"),
        (PageWaitRequestState::Changed, PageWaitRelation::Unqualified, "view.page_waits.recheck.changed"),
    ] {
        let mut row = observation("61", WaitResourceKind::MemoryPage);
        let original = row.request.clone();
        row.after_check.as_mut().unwrap().request = request;
        row.after_check.as_mut().unwrap().wait = wait;
        if request != PageWaitRequestState::SameStatement {
            row.statement_identity = None;
        }
        let report = explain(&[&row], None).unwrap();
        assert!(report.entries[0].explanation.contains(&culprit_core::i18n::t(key, &[])));
        assert!(report.entries[0].explanation.contains("app.orders"));
        assert_eq!(row.request, original);
        assert_eq!(report.entries[0].resource, row.resource);
    }
}

#[test]
fn empty_partial_unavailable_and_unknown_sources_never_establish_absence_of_parallel_waits() {
    for state in [SourceState::Empty, SourceState::Partial, SourceState::Unavailable, SourceState::NotApplicable] {
        let mut native = source(state);
        native.reason = Some("native visibility unavailable".into());
        let report = explain(&[], Some(&native)).unwrap();
        assert!(report.entries.is_empty());
        assert_eq!(report.summary, t!("view.page_waits.summary.empty"));
        assert_eq!(report.source_state, Some(state));
        assert_eq!(report.source_reason, native.reason);
    }
    let report = explain(&[], None).unwrap();
    assert!(report.limitations.contains(&t!("view.page_waits.limit.source_unknown")));
}

#[test]
fn malformed_rows_conflicting_coverage_and_duplicate_request_resources_cannot_enter_the_view() {
    let row = observation("61", WaitResourceKind::MemoryPage);
    assert!(explain(&[&row, &row], None).is_err());
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::NotApplicable] {
        assert!(explain(&[&row], Some(&source(state))).is_err());
    }
    assert!(explain(&[], Some(&source(SourceState::Collected))).is_err());
    let mut wrong = source(SourceState::Partial);
    wrong.source = "a different native source".into();
    assert!(explain(&[&row], Some(&wrong)).is_err());
    let mut changed = row.clone();
    changed.request.wait = None;
    assert!(explain(&[&changed], None).is_err());
    let too_many = vec![&row; MAX_PAGE_WAIT_OBSERVATIONS + 1];
    assert!(explain(&too_many, None).is_err());
}

#[test]
fn explanations_localize_without_reinterpreting_the_native_wait_or_catalog_name() {
    let row = observation("61", WaitResourceKind::MemoryPage);
    for language in ["en", "es", "fr", "de"] {
        culprit_core::i18n::scope(language, || {
            let report = explain(&[&row], None).unwrap();
            assert_eq!(report.title, t!("view.page_waits.title"));
            assert!(report.entries[0].explanation.contains("ix_orders"));
            assert_eq!(report.entries[0].request_key, row.request.key);
            assert_eq!(report.entries[0].resource, row.resource);
        });
    }
}

#[test]
fn another_page_header_or_missing_observed_address_cannot_be_explained_as_this_wait_target() {
    for field in ["database", "file", "page", "missing-address"] {
        let mut row = observation("61", WaitResourceKind::MemoryPage);
        match field {
            "database" => row.target.as_mut().unwrap().header.database_id = 99,
            "file" => row.target.as_mut().unwrap().header.file_id = 99,
            "page" => row.target.as_mut().unwrap().header.page_id = 99,
            "missing-address" => row.observed_address = None,
            _ => unreachable!(),
        }
        assert!(explain(&[&row], Some(&source(SourceState::Collected))).is_err(), "{field}");
    }
}

#[test]
fn undecoded_native_wait_address_preserves_the_original_participant_without_an_object_mapping() {
    let mut row = observation("61", WaitResourceKind::MemoryPage);
    row.resource.native.id = "opaque-unrecognized-page".into();
    row.request.wait.as_mut().unwrap().identified_resources = vec![row.resource.clone()];
    row.observed_address = None;
    row.target = None;
    row.resolution = PageWaitResolution::UnrecognizedAddress;
    let report = explain(&[&row], Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].request_key, row.request.key);
    assert_eq!(report.entries[0].resource, row.resource);
    assert!(!report.entries[0].explanation.contains("ix_orders"));
}
