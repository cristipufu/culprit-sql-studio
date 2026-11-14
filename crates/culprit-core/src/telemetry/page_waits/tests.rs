use super::*;
use crate::Engine;
mod index_operations;

const SOURCE: &str = crate::source_id::PAGE_WAIT_TARGETS;

fn resource() -> ObservedWaitResource {
    ObservedWaitResource {
        kind: WaitResourceKind::MemoryPage,
        native: NativeIdentity::new("5:1:42").qualified("source", "sys.dm_exec_requests.wait_resource"),
    }
}

fn target() -> MappedPageTarget {
    MappedPageTarget {
        identity_basis: PageTargetIdentityBasis::LocalDatabaseFileEpoch,
        database: NativeIdentity::new("5").qualified("database_guid", "native-database-guid"),
        file: NativeIdentity::new("1").qualified("file_guid", "native-file-guid"),
        header: PageHeaderFacts {
            database_id: 5,
            file_id: 1,
            page_id: 42,
            header_version: Some(1),
            page_type: Some(2),
            level: Some(0),
            object_id: Some("128".into()),
            index_id: Some("1".into()),
            partition_id: None,
            allocation_unit_id: Some("72057594000000001".into()),
            lsn: Some("00000001:00000020:0001".into()),
            next_file_id: None,
            next_page_id: None,
        },
        kind: PageWaitTargetKind::IndexLeaf,
        object: Some(PageWaitObject {
            native: NativeIdentity::new("128"),
            schema: "dbo".into(),
            name: "orders".into(),
            index_name: Some("orders_id".into()),
        }),
    }
}

fn observation() -> PageWaitObservation {
    let resource = resource();
    let mut request = super::super::activity::tests::request("42");
    request.wait = Some(ActivityWait {
        native_type: Some("PAGELATCH_EX".into()),
        elapsed_ms: Some("1000".into()),
        owners: vec![ActivityWaitOwner::Unknown { native_id: "-5".into(), reason: UnknownWaitOwner::UntrackedLatch }],
        resources: None,
        identified_resources: vec![resource.clone()],
    });
    PageWaitObservation {
        source: SOURCE.into(),
        observed_from: "2026-11-13T10:00:00.200000Z".into(),
        observed_until: "2026-11-13T10:00:00.800000Z".into(),
        request,
        before_check: Some(PageWaitRecheck {
            observed_at: "2026-11-13T10:00:00.3Z".into(),
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        }),
        after_check: Some(PageWaitRecheck {
            observed_at: "2026-11-13T10:00:00.7Z".into(),
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        }),
        statement_identity: Some(NativeIdentity::new("native-sql-handle").qualified("statement_start_offset", "0")),
        plan_identity: Some(NativeIdentity::new("native-plan-handle")),
        resource,
        observed_address: Some(PageWaitAddress { database_id: 5, file_id: 1, page_id: 42 }),
        resolution: PageWaitResolution::ObservedAtBothBounds,
        target: Some(Box::new(target())),
        index_operations: None,
    }
}

fn snapshot() -> PageWaitSnapshot {
    PageWaitSnapshot {
        source: SOURCE.into(),
        observed_from: "2026-11-13T10:00:00.100000Z".into(),
        observed_until: "2026-11-13T10:00:00.900000Z".into(),
        observations: vec![observation()],
    }
}

#[test]
fn mapped_page_evidence_preserves_the_original_waiter_resource_and_unknown_latch_owner() {
    for (kind, native_wait) in
        [(WaitResourceKind::MemoryPage, "PAGELATCH_EX"), (WaitResourceKind::PageIo, "PAGEIOLATCH_SH")]
    {
        let mut value = observation();
        value.resource.kind = kind;
        let wait = value.request.wait.as_mut().unwrap();
        wait.native_type = Some(native_wait.into());
        wait.identified_resources = vec![value.resource.clone()];
        assert!(value.is_valid());
        let roundtrip: PageWaitObservation = serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(roundtrip, value);
        assert_eq!(roundtrip.request.wait.as_ref().unwrap().identified_resources, [value.resource]);
        assert!(matches!(
            roundtrip.request.wait.as_ref().unwrap().owners.as_slice(),
            [ActivityWaitOwner::Unknown { reason: UnknownWaitOwner::UntrackedLatch, .. }]
        ));
        assert_eq!(roundtrip.target.as_ref().unwrap().header.page_id, 42);
    }
}

#[test]
fn page_mapping_cannot_attach_a_resource_that_the_original_request_did_not_observe() {
    let mut value = observation();
    value.request.wait = None;
    assert!(!value.is_valid());
    let mut value = observation();
    value.resource.native.id = "5:1:43".into();
    assert!(!value.is_valid());
    let mut value = observation();
    value.resource.kind = WaitResourceKind::DataLock;
    value.request.wait.as_mut().unwrap().identified_resources = vec![value.resource.clone()];
    assert!(!value.is_valid(), "a lock resource does not acquire memory-page semantics");
    let mut value = observation();
    value.request.key.request = None;
    value.request.request_counters = None;
    assert!(!value.is_valid(), "session context alone cannot establish the original affected request");
}

#[test]
fn unresolved_mapping_keeps_original_context_but_never_publishes_a_target() {
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
        let mut value = observation();
        value.resolution = resolution;
        assert!(!value.is_valid(), "{resolution:?} cannot retain a mapped target");
        value.target = None;
        value.statement_identity = None;
        value.plan_identity = None;
        assert!(value.is_valid(), "{resolution:?} retains the originally observed request");
        assert_eq!(value.request.key, observation().request.key);
    }
    let mut value = observation();
    value.target = None;
    assert!(!value.is_valid(), "a successful mapping needs its native target evidence");
}

#[test]
fn native_statement_links_need_a_checked_request_and_plan_links_need_a_statement() {
    for state in [PageWaitRequestState::Missing, PageWaitRequestState::Changed, PageWaitRequestState::Unverified] {
        let mut value = observation();
        value.after_check.as_mut().unwrap().request = state;
        value.after_check.as_mut().unwrap().wait = PageWaitRelation::Unqualified;
        assert!(!value.is_valid(), "{state:?} cannot publish statement or plan handles");
        value.plan_identity = None;
        assert!(!value.is_valid(), "{state:?} cannot publish a statement handle alone");
        value.statement_identity = None;
        assert!(
            value.is_valid(),
            "qualified current header remains useful when request continuity cannot be established"
        );
    }
    let mut value = observation();
    value.statement_identity = None;
    assert!(!value.is_valid(), "a plan handle cannot independently qualify its statement");
    value.plan_identity = None;
    value.before_check = None;
    value.after_check = None;
    assert!(value.is_valid(), "a native page header remains useful without statement or plan identity");
}

#[test]
fn matching_request_checks_cannot_authorize_statement_links_from_an_unverified_source() {
    let mut value = observation();
    value.target = None;
    value.resolution = PageWaitResolution::UnverifiedSource;
    assert!(!value.is_valid(), "matching request checks do not establish native source authority");
    value.plan_identity = None;
    assert!(!value.is_valid(), "an unverified source cannot publish a statement handle alone");
    value.statement_identity = None;
    assert!(value.is_valid(), "the original observed wait remains useful without unqualified handles");

    for resolution in
        [PageWaitResolution::Unsupported, PageWaitResolution::Unavailable, PageWaitResolution::PermissionDenied]
    {
        let mut value = observation();
        value.target = None;
        value.resolution = resolution;
        assert!(value.is_valid(), "{resolution:?} need not invalidate independent native statement proof");
    }
}

#[test]
fn a_completed_or_changed_short_wait_does_not_erase_its_original_participant_or_qualified_current_header() {
    let original = observation();
    for relation in [PageWaitRelation::NoLongerObserved, PageWaitRelation::Different] {
        let mut value = original.clone();
        value.before_check.as_mut().unwrap().wait = relation;
        value.after_check.as_mut().unwrap().wait = relation;
        assert!(value.is_valid());
        assert_eq!(value.request, original.request);
        assert_eq!(value.resource, original.resource);
        assert_eq!(value.key().unwrap(), original.key().unwrap());
    }
}

#[test]
fn recheck_clocks_and_original_wait_relationship_cannot_be_forged_across_a_changed_request() {
    for at in ["2026-11-13T10:00:00.1Z", "2026-11-13T10:00:00.9Z", "not-a-clock"] {
        let mut value = observation();
        value.after_check.as_mut().unwrap().observed_at = at.into();
        assert!(!value.is_valid(), "{at}");
    }
    let mut value = observation();
    value.before_check.as_mut().unwrap().observed_at = "2026-11-13T10:00:00.75Z".into();
    assert!(!value.is_valid(), "before cannot follow after");
    let mut check = observation().before_check.unwrap();
    check.request = PageWaitRequestState::Changed;
    assert!(!check.is_valid(), "a different request cannot establish whether the original wait persists");
    check.wait = PageWaitRelation::Unqualified;
    assert!(check.is_valid());
}

#[test]
fn only_request_database_scope_can_acquire_current_page_metadata() {
    for scope in [
        ActivityScopeBasis::SessionDatabase,
        ActivityScopeBasis::TransactionDatabase,
        ActivityScopeBasis::BlockingReference,
        ActivityScopeBasis::OtherDatabase,
    ] {
        let mut value = observation();
        value.request.scope = scope;
        assert!(!value.is_valid(), "{scope:?} cannot publish a database-qualified mapping");
        value.target = None;
        value.resolution = PageWaitResolution::OutsideDatabase;
        assert!(value.is_valid(), "the original affected request remains useful when mapping is withheld");
    }
}

#[test]
fn native_signed_header_identifiers_remain_exact_and_catalog_absence_stays_unknown() {
    let mut value = observation();
    let target = value.target.as_mut().unwrap();
    target.object = None;
    target.header.object_id = Some(i32::MIN.to_string());
    target.header.index_id = Some("-1".into());
    target.header.partition_id = Some(i64::MIN.to_string());
    target.header.allocation_unit_id = Some(i64::MAX.to_string());
    assert!(value.is_valid());
    let decoded: PageWaitObservation = serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
    let header = &decoded.target.as_ref().unwrap().header;
    assert_eq!(header.object_id.as_deref(), Some("-2147483648"));
    assert_eq!(header.partition_id.as_deref(), Some("-9223372036854775808"));
    assert_eq!(header.allocation_unit_id.as_deref(), Some("9223372036854775807"));
    assert!(decoded.target.as_ref().unwrap().object.is_none());
    assert!(header.next_page_id.is_none());
    for invalid in ["9223372036854775808", "-9223372036854775809", "NaN", "1.5"] {
        let mut invalid_header = decoded.clone();
        invalid_header.target.as_mut().unwrap().header.allocation_unit_id = Some(invalid.into());
        assert!(!invalid_header.is_valid(), "{invalid}");
    }
}

#[test]
fn counter_progress_and_page_lsn_changes_do_not_replace_the_native_wait_row_identity() {
    let mut value = observation();
    let original = value.key().unwrap();
    value.request.request_counters.as_mut().unwrap().cpu_ms = Some("9007199254740993".into());
    value.request.wait.as_mut().unwrap().elapsed_ms = Some("2000".into());
    value.target.as_mut().unwrap().header.lsn = Some("00000002:00000030:0002".into());
    value.observed_until = "2026-11-13T10:00:00.850000Z".into();
    assert!(value.is_valid());
    assert_eq!(value.key().unwrap(), original);
    let mut replaced_request = value.clone();
    replaced_request.request.key.request.as_mut().unwrap().generation = "request-2".into();
    assert_ne!(replaced_request.key().unwrap(), original);
    value.resource.native.id = "5:1:43".into();
    value.observed_address.as_mut().unwrap().page_id = 43;
    value.request.wait.as_mut().unwrap().identified_resources = vec![value.resource.clone()];
    value.target = None;
    value.resolution = PageWaitResolution::ChangedMetadata;
    assert!(value.is_valid());
    assert_ne!(value.key().unwrap(), original);
}

#[test]
fn capture_validation_rejects_outside_bounds_source_changes_and_duplicate_native_waits() {
    assert!(snapshot().is_valid());
    for (from, until) in [
        ("2026-11-13T10:00:00Z", "2026-11-13T10:00:00.800000Z"),
        ("2026-11-13T10:00:00.200000Z", "2026-11-13T10:00:01Z"),
        ("2026-11-13T10:00:00.800000Z", "2026-11-13T10:00:00.200000Z"),
    ] {
        let mut value = snapshot();
        value.observations[0].observed_from = from.into();
        value.observations[0].observed_until = until.into();
        assert!(!value.is_valid(), "{from}..{until}");
    }
    let mut value = snapshot();
    value.observations[0].source = "different native source".into();
    assert!(!value.is_valid());
    let mut value = snapshot();
    let mut progressed = value.observations[0].clone();
    progressed.request.elapsed_ms = Some("200000".into());
    value.observations.push(progressed);
    assert!(!value.is_valid(), "counter progress cannot create a second row with the same wait identity");
}

#[test]
fn ordinary_redaction_preserves_native_client_context_and_the_payload_inventory_uses_the_same_statement() {
    let mut value = observation();
    let sql = "SELECT * FROM orders WHERE name=N'page-query-canary'";
    value.request.statement = DefinitionPayload::Available {
        text: sql.into(),
        sha256: digest(sql.as_bytes()),
        redaction_version: REDACTION_VERSION,
    };
    value.request.client = Some(ActivityClient {
        application: Some("page-application-canary".into()),
        user: Some("page-user-canary".into()),
        host: Some("page-host-canary".into()),
    });
    let key = value.key().unwrap();
    value.redact(Engine::Mssql);
    assert!(value.is_valid());
    assert_eq!(value.key().unwrap(), key);
    assert_eq!(value.request.client.as_ref().unwrap().application.as_deref(), Some("page-application-canary"));
    assert!(
        matches!(&value.request.statement, DefinitionPayload::Available { text, .. } if !text.contains("page-query-canary"))
    );
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: SOURCE.into(),
        key,
        fact: OperationalFact::PageWait(Box::new(value)),
    });
    assert!(record.is_valid());
    let fields = record.definition_payloads().collect::<Vec<_>>();
    assert_eq!(fields.len(), 1);
    let json = serde_json::json!({"record": &record});
    assert_eq!(json.pointer(fields[0].pointer).unwrap(), &serde_json::to_value(fields[0].payload).unwrap());
}

#[test]
fn snapshot_sharing_strips_page_wait_clients_and_physical_metadata_without_changing_collected_evidence() {
    let mut original: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut evidence = snapshot();
    let row = &mut evidence.observations[0];
    let sql = "SELECT * FROM orders WHERE name=N'page-query-canary'";
    row.request.statement = DefinitionPayload::Available {
        text: sql.into(),
        sha256: digest(sql.as_bytes()),
        redaction_version: REDACTION_VERSION,
    };
    row.request.client = Some(ActivityClient {
        application: Some("page-application-canary".into()),
        user: Some("page-user-canary".into()),
        host: Some("page-host-canary".into()),
    });
    original.operations.page_waits = Some(evidence.clone());
    let shared = crate::redact::redact_snapshot(&original);
    let serialized = serde_json::to_string(&shared).unwrap();
    assert!(!serialized.contains("-canary"));
    assert!(!serialized.contains("native-database-guid"));
    assert!(!serialized.contains("native-file-guid"));
    let shared_evidence = shared.operations.page_waits.as_ref().unwrap();
    assert!(shared_evidence.is_valid());
    let shared_row = &shared_evidence.observations[0];
    assert!(shared_row.request.client.is_none());
    assert_eq!(shared_row.request.key, evidence.observations[0].request.key);
    assert_eq!(shared_row.resource, evidence.observations[0].resource);
    assert_eq!(shared_row.target.as_ref().unwrap().header, evidence.observations[0].target.as_ref().unwrap().header);
    assert_eq!(original.operations.page_waits.as_ref().unwrap(), &evidence);
    assert_eq!(
        serde_json::to_value(&shared).unwrap(),
        serde_json::to_value(crate::redact::redact_snapshot(&shared)).unwrap()
    );
}

#[test]
fn mapped_headers_require_the_source_decoded_original_database_file_and_page() {
    for field in ["database", "file", "page", "missing-address", "invalid-database", "invalid-file"] {
        let mut value = observation();
        match field {
            "database" => value.target.as_mut().unwrap().header.database_id = 99,
            "file" => value.target.as_mut().unwrap().header.file_id = 99,
            "page" => value.target.as_mut().unwrap().header.page_id = 99,
            "missing-address" => value.observed_address = None,
            "invalid-database" => value.observed_address.as_mut().unwrap().database_id = 0,
            "invalid-file" => value.observed_address.as_mut().unwrap().file_id = 0,
            _ => unreachable!(),
        }
        assert!(!value.is_valid(), "{field}");
        let mut captured = snapshot();
        captured.observations = vec![value];
        assert!(!captured.is_valid(), "{field}");
    }
    let mut value = observation();
    value.observed_address.as_mut().unwrap().page_id = 0;
    value.target.as_mut().unwrap().header.page_id = 0;
    assert!(value.is_valid(), "native page zero is a valid address, not an absent coordinate");
}

#[test]
fn unresolved_undecoded_native_addresses_remain_original_evidence_without_header_access() {
    let mut value = observation();
    value.resource.native.id = "unrecognized-native-page".into();
    value.request.wait.as_mut().unwrap().identified_resources = vec![value.resource.clone()];
    value.observed_address = None;
    value.resolution = PageWaitResolution::UnrecognizedAddress;
    value.target = None;
    assert!(value.is_valid());
    let original = value.request.clone();
    let serialized = serde_json::to_string(&value).unwrap();
    assert!(!serialized.contains("observed_address"));
    let restored: PageWaitObservation = serde_json::from_str(&serialized).unwrap();
    assert_eq!(restored.request, original);
    assert_eq!(restored.observed_address, None);
    value.resolution = PageWaitResolution::ObservedAtBothBounds;
    value.target = Some(Box::new(target()));
    assert!(!value.is_valid(), "an undecoded wait cannot acquire another page's header");
}

#[test]
fn coordinate_validation_does_not_parse_source_native_tokens_or_compare_incarnation_guids_to_ordinals() {
    let mut value = observation();
    value.resource.native = NativeIdentity::new("opaque-page-token").qualified("provider", "custom-native-layout");
    value.request.wait.as_mut().unwrap().identified_resources = vec![value.resource.clone()];
    let target = value.target.as_mut().unwrap();
    target.database = NativeIdentity::new("opaque-database-incarnation-guid");
    target.file = NativeIdentity::new("opaque-file-incarnation-guid");
    assert!(value.is_valid());
    assert_eq!(value.observed_address, Some(PageWaitAddress { database_id: 5, file_id: 1, page_id: 42 }));
}
