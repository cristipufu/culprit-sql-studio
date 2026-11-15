use super::*;
use culprit_core::{ObservedWaitResource, WaitResourceKind};

pub fn page_wait_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let (mut manifest, _) = fixture();
    let source_id = culprit_core::source_id::PAGE_WAIT_TARGETS;
    manifest.source.source = "page-target-capture".into();
    let first = observation("61", "5:1:42", WaitResourceKind::MemoryPage, 42);
    let mut io = observation("62", "5:1:55", WaitResourceKind::PageIo, 55);
    io.request.command = Some("SELECT".into());
    io.request.statement =
        DefinitionPayload::sql("SELECT payload FROM dbo.orders WHERE customer_id=9264", culprit_core::Engine::Mssql);
    io.after_check.as_mut().unwrap().wait = PageWaitRelation::NoLongerObserved;
    let full = vec![record(first), record(io)];
    let mut partial = full.clone();
    for (id, address, page, resolution, request) in [
        ("63", "5:1:66", 66, PageWaitResolution::ObservedAtBothBounds, PageWaitRequestState::Changed),
        ("64", "5:1:77", 77, PageWaitResolution::Unavailable, PageWaitRequestState::Missing),
        ("65", "5:1:88", 88, PageWaitResolution::UnverifiedSource, PageWaitRequestState::Unverified),
    ] {
        let mut unresolved = observation(id, address, WaitResourceKind::MemoryPage, page);
        unresolved.resolution = resolution;
        unresolved.after_check.as_mut().unwrap().request = request;
        unresolved.after_check.as_mut().unwrap().wait = PageWaitRelation::Unqualified;
        if resolution != PageWaitResolution::ObservedAtBothBounds {
            unresolved.target = None;
        }
        unresolved.statement_identity = None;
        unresolved.plan_identity = None;
        partial.push(record(unresolved));
    }
    let mut undecoded = observation("66", "unparsed-native-page", WaitResourceKind::MemoryPage, 0);
    undecoded.observed_address = None;
    undecoded.resolution = PageWaitResolution::UnrecognizedAddress;
    undecoded.target = None;
    undecoded.before_check = None;
    undecoded.after_check = None;
    undecoded.statement_identity = None;
    undecoded.plan_identity = None;
    partial.push(record(undecoded));
    let mut cases = vec![
        (full, culprit_core::SourceState::Collected),
        (partial, culprit_core::SourceState::Partial),
        (vec![], culprit_core::SourceState::Empty),
        (vec![], culprit_core::SourceState::Unavailable),
    ];
    for relation in [
        PageIndexOperationRelation::CheckedTarget,
        PageIndexOperationRelation::ChangedTarget,
        PageIndexOperationRelation::UnverifiedTarget,
    ] {
        let mut observed = observation("67", "5:1:42", WaitResourceKind::MemoryPage, 42);
        observed.after_check.as_mut().unwrap().wait = PageWaitRelation::NoLongerObserved;
        observed.index_operations = Some(index_point(observed.target.as_ref().unwrap(), relation));
        if relation != PageIndexOperationRelation::CheckedTarget {
            observed.target = None;
            observed.resolution = PageWaitResolution::ChangedMetadata;
        }
        let state = if relation == PageIndexOperationRelation::CheckedTarget {
            culprit_core::SourceState::Collected
        } else {
            culprit_core::SourceState::Partial
        };
        cases.push((vec![record(observed)], state));
    }
    for point in [PageIndexOperations::Missing, PageIndexOperations::PermissionDenied, PageIndexOperations::Unavailable]
    {
        let mut observed = observation("68", "5:1:42", WaitResourceKind::MemoryPage, 42);
        observed.index_operations = Some(point);
        cases.push((vec![record(observed)], culprit_core::SourceState::Partial));
    }
    cases.extend(task_wait_cases());
    let mut cases = cases
        .into_iter()
        .map(|(records, state)| {
            let mut source = manifest.source.clone();
            source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitTargets,
                observed_from: "2020-01-01T12:00:00Z".into(),
                observed_until: "2020-01-01T12:00:01Z".into(),
                records: records.len() as u32,
                sources: vec![if state == culprit_core::SourceState::Unavailable {
                    StateSourceCoverage::unavailable(source_id.into(), culprit_core::ErrorCode::PermissionDenied, None)
                } else {
                    StateSourceCoverage::observed(
                        source_id.into(),
                        records.len() as u32,
                        state != culprit_core::SourceState::Partial,
                    )
                }],
            });
            (source, records)
        })
        .collect::<Vec<_>>();
    cases.extend(shared_page_wait_evidence_cases());
    cases
}

/// Source-owned shared snapshots retain the original application waiter. Names are privacy canaries; no
/// current native owner, cache population or causal writer responsibility is inferred from this fixture.
pub fn shared_page_wait_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let (manifest, _) = fixture();
    let resource = SharedResourceIdentity {
        engine: culprit_core::Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("private-tempdb-machine-canary"),
        native: NativeIdentity::new("private-tempdb-instance-canary"),
    };
    let partition = SourcePartition::InstanceLocal {
        member: Some(resource.clone()),
        epoch: Some("native-tempdb-process-epoch".into()),
    };
    let mut first = observation("81", "2:1:42", WaitResourceKind::MemoryPage, 42);
    first.request.statement_identity = first.statement_identity.clone();
    first.observed_address.as_mut().unwrap().database_id = 2;
    let scope = PageTargetScope::SharedResource {
        context: Box::new(NativeReadContext {
            target: CollectionTarget::Shared(resource.clone()),
            partition: partition.clone(),
            observed_from: "2020-01-01T12:00:00.100000000Z".into(),
            observed_until: first.observed_until.clone(),
            database_writable: None,
        }),
    };
    let target = first.target.as_mut().unwrap();
    target.header.database_id = 2;
    target.header.page_type = Some(11);
    target.header.object_id = None;
    target.header.index_id = None;
    target.header.partition_id = None;
    target.object = None;
    target.kind = PageWaitTargetKind::Pfs;
    let (database, file) = scope
        .bind_identities(
            NativeIdentity::new("2")
                .qualified("database_guid", "native-tempdb-guid")
                .qualified("created_without_offset", "2020-01-01T04:00:00.0000000"),
            NativeIdentity::new("native-tempdb-file").qualified("create_lsn", "0"),
        )
        .unwrap();
    target.scope = scope;
    target.database = database;
    target.file = file;
    first.index_operations = Some(PageIndexOperations::Unavailable);
    let mut source = manifest.source;
    source.target = CollectionTarget::Shared(resource);
    source.partition = partition;
    source.source = "shared-page-target-capture".into();
    source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::PageWaitTargets,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        records: 1,
        sources: vec![StateSourceCoverage::observed(culprit_core::source_id::PAGE_WAIT_TARGETS.into(), 1, false)],
    });
    let mut empty = source.clone();
    let SourceObservation::Snapshot(snapshot) = &mut empty.observation else { unreachable!() };
    snapshot.records = 0;
    snapshot.sources[0] = StateSourceCoverage::observed(culprit_core::source_id::PAGE_WAIT_TARGETS.into(), 0, false);
    let mut unavailable = empty.clone();
    let SourceObservation::Snapshot(snapshot) = &mut unavailable.observation else { unreachable!() };
    snapshot.sources[0] = StateSourceCoverage::unavailable(
        culprit_core::source_id::PAGE_WAIT_TARGETS.into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    );
    let mut focused = source.clone();
    let mut snapshot = source.observation.snapshot().unwrap().clone();
    snapshot.observed_from = "2020-01-01T12:00:00.050000000Z".into();
    let (mut parent, _) = activity_evidence_cases().remove(0);
    parent.source = culprit_core::source_id::SESSIONS.into();
    parent.partition = SourcePartition::InstanceLocal {
        member: match &source.partition {
            SourcePartition::InstanceLocal { member, .. } => member.clone(),
            _ => unreachable!(),
        },
        epoch: Some("native-original-database-epoch".into()),
    };
    let SourceObservation::Activity(frame) = &mut parent.observation else { unreachable!() };
    frame.epoch = Some("native-original-database-epoch".into());
    frame.database_writable = Some(true);
    frame.native_rows = 1;
    frame.observations = 1;
    frame.coverage = StateSourceCoverage::observed(parent.source.clone(), 1, false);
    let original_at = frame.at.clone();
    let original_capture = Recovery::capture(
        "fixture:page-parent",
        &digest(b"fixture-schema"),
        parent,
        vec![TelemetryRecord::from(first.request.clone())],
        920,
    );
    let manifest = &original_capture.manifest;
    let original =
        ExportedRecord::new(&manifest.source, &manifest.capture, TelemetryRecord::from(first.request.clone())).unwrap();
    let focus = ActivityWaitFocus {
        target: manifest.source.target.clone(),
        source: manifest.source.source.clone(),
        partition: manifest.source.partition.clone(),
        database_writable: Some(true),
        epoch: "native-original-database-epoch".into(),
        original_observed_at: original_at,
        parent: DiagnosticEvidenceRef::from_export(&manifest.source, &manifest.capture_manifest_sha256, &original, 0)
            .unwrap(),
        statement_identity: first.request.statement_identity.clone().unwrap(),
        resource: first.resource.clone(),
        original: first.request.clone(),
    };
    focused.observation = SourceObservation::FocusedPageWait {
        snapshot: snapshot.clone(),
        result: Box::new(ActivityWaitFocusResult {
            focus,
            observed_from: snapshot.observed_from.clone(),
            observed_until: snapshot.observed_until.clone(),
            source: FocusSourceState::Matched,
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        }),
    };
    vec![(source, vec![record(first.clone())]), (empty, vec![]), (unavailable, vec![]), (focused, vec![record(first)])]
}

fn task_wait_cases() -> Vec<(Vec<TelemetryRecord>, culprit_core::SourceState)> {
    let first = parallel_observation("71", WaitResourceKind::MemoryPage, 42, 1);
    let mut second = first.clone();
    let task = second.task_wait.as_mut().unwrap();
    task.execution_context_id = 2;
    task.task_address = "0000000000001002".into();
    task.wait_duration_ms = Some("0".into());
    let mut io = parallel_observation("71", WaitResourceKind::PageIo, 55, 3);
    io.request.wait = None;
    let parallel = vec![record(first), record(second), record(io)];

    let owner_context = [Some(-2), Some(-3), Some(-4), Some(-99), Some(0), Some(91), None]
        .into_iter()
        .enumerate()
        .map(|(ordinal, owner)| {
            let mut row = parallel_observation("72", WaitResourceKind::MemoryPage, 42, ordinal as u32 + 1);
            let task = row.task_wait.as_mut().unwrap();
            task.blocking_session_id = owner;
            task.blocking_execution_context_id = owner.map(|_| -1);
            task.blocking_task_address = owner.map(|_| "0000000000000000".into());
            record(row)
        })
        .collect();

    let finished = [
        (PageWaitRequestState::SameStatement, PageWaitRelation::NoLongerObserved),
        (PageWaitRequestState::SameStatement, PageWaitRelation::Different),
        (PageWaitRequestState::Changed, PageWaitRelation::Unqualified),
    ]
    .into_iter()
    .enumerate()
    .map(|(ordinal, (request, relation))| {
        let mut row = parallel_observation("73", WaitResourceKind::MemoryPage, 42, ordinal as u32 + 1);
        row.after_check.as_mut().unwrap().wait = relation;
        row.after_check.as_mut().unwrap().request = request;
        row.statement_identity = None;
        row.plan_identity = None;
        row.index_operations =
            Some(index_point(row.target.as_ref().unwrap(), PageIndexOperationRelation::CheckedTarget));
        record(row)
    })
    .collect();
    vec![
        (parallel, culprit_core::SourceState::Collected),
        (owner_context, culprit_core::SourceState::Collected),
        (finished, culprit_core::SourceState::Partial),
    ]
}

fn parallel_observation(session: &str, kind: WaitResourceKind, page: u32, context: u32) -> PageWaitObservation {
    let mut row = observation(session, &format!("5:1:{page}"), kind, page);
    row.resource.native =
        NativeIdentity::new(format!("5:1:{page}")).qualified("source", "sys.dm_os_waiting_tasks.resource_description");
    row.task_wait = Some(MssqlPageTaskWait {
        observed_at: row.observed_from.clone(),
        session_id: session.parse().unwrap(),
        request_id: row.request.key.request.as_ref().unwrap().id.parse().unwrap(),
        execution_context_id: context,
        task_address: format!("{:016X}", 0x1000 + context),
        wait_type: match kind {
            WaitResourceKind::MemoryPage => "PAGELATCH_EX",
            WaitResourceKind::PageIo => "PAGEIOLATCH_SH",
            WaitResourceKind::DataLock => unreachable!(),
        }
        .into(),
        wait_duration_ms: Some("250".into()),
        resource: row.resource.clone(),
        resource_address: Some("0000000000002000".into()),
        blocking_task_address: None,
        blocking_session_id: Some(-4),
        blocking_execution_context_id: None,
    });
    row.request.wait = Some(ActivityWait {
        native_type: Some("CXCONSUMER".into()),
        elapsed_ms: Some("17".into()),
        owners: vec![],
        resources: None,
        identified_resources: vec![],
    });
    row
}

fn index_point(target: &MappedPageTarget, relation: PageIndexOperationRelation) -> PageIndexOperations {
    let verified_catalog = relation != PageIndexOperationRelation::UnverifiedTarget;
    PageIndexOperations::Observed {
        observed_at: "2020-01-01T12:00:00.500000000Z".into(),
        partition_id: verified_catalog.then(|| target.header.partition_id.clone()).flatten(),
        object: verified_catalog.then(|| target.object.as_ref().map(|object| object.native.clone())).flatten(),
        relation,
        counters: Box::new(MssqlIndexOperations {
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
            reset: culprit_core::ResetMarker::Unknown,
        }),
    }
}

pub fn page_wait_tamper_cases(original: &serde_json::Value) -> Vec<serde_json::Value> {
    use serde_json::json;
    let changed_wait = if original.pointer("/after_check/wait") == Some(&json!("no-longer-observed")) {
        json!("original-observed")
    } else {
        json!("no-longer-observed")
    };
    let changed_link = if original.pointer("/index_operations/relation") == Some(&json!("checked-target")) {
        json!("unverified-target")
    } else {
        json!("checked-target")
    };
    let changed_owner =
        if original.pointer("/task_wait/blocking_session_id") == Some(&json!(91)) { json!(-4) } else { json!(91) };
    [
        ("/target/header/page_id", json!(999)),
        ("/target/scope/context/target/key", json!(digest(b"wrong shared owner"))),
        ("/target/scope/context/partition/key", json!(digest(b"wrong native epoch"))),
        ("/target/scope/context/observed_from", json!("2020-01-01T11:59:59Z")),
        ("/target/scope/context/observed_until", json!("2020-01-01T12:00:02Z")),
        ("/target/database/qualifiers/database_guid", json!("wrong-tempdb-incarnation")),
        ("/target/file/qualifiers/page_target_database", json!(digest(b"wrong tempdb file namespace"))),
        ("/observed_address/page_id", json!(999)),
        ("/after_check/wait", changed_wait),
        ("/before_check/observed_at", json!("2020-01-01T12:00:00.400000000Z")),
        ("/index_operations/observed_at", json!("2020-01-01T12:00:00.600000000Z")),
        ("/index_operations/counters/identity/partition_number", json!(2)),
        ("/index_operations/counters/counters/page-latch-wait-ms", json!("0")),
        ("/index_operations/relation", changed_link),
        ("/task_wait/observed_at", json!("2020-01-01T12:00:00.200000000Z")),
        ("/task_wait/session_id", json!(99)),
        ("/task_wait/request_id", json!(99)),
        ("/task_wait/execution_context_id", json!(99)),
        ("/task_wait/task_address", json!("0000000000009999")),
        ("/task_wait/wait_duration_ms", json!("123")),
        ("/task_wait/resource/native/id", json!("5:1:999")),
        ("/task_wait/resource_address", json!("0000000000009999")),
        ("/task_wait/blocking_task_address", json!("0000000000009999")),
        ("/task_wait/blocking_session_id", changed_owner),
        ("/task_wait/blocking_execution_context_id", json!(-99)),
    ]
    .into_iter()
    .filter_map(|(pointer, replacement)| {
        let mut changed = original.clone();
        let value = changed.pointer_mut(pointer)?;
        assert_ne!(*value, replacement, "a conformance mutation must change the committed fact: {pointer}");
        *value = replacement;
        Some(changed)
    })
    .collect()
}

fn observation(session: &str, address: &str, kind: WaitResourceKind, page: u32) -> PageWaitObservation {
    let resource = ObservedWaitResource {
        kind,
        native: NativeIdentity::new(address).qualified("source", "sys.dm_exec_requests.wait_resource"),
    };
    let mut request = activity_sample();
    request.key.session.id = session.into();
    request.state = "suspended".into();
    request.command = Some("INSERT".into());
    request.statement =
        DefinitionPayload::sql("INSERT INTO dbo.orders(customer_id) VALUES (9264)", culprit_core::Engine::Mssql);
    request.wait = Some(ActivityWait {
        identified_resources: vec![resource.clone()],
        native_type: Some(
            match kind {
                WaitResourceKind::MemoryPage => "PAGELATCH_EX",
                WaitResourceKind::PageIo => "PAGEIOLATCH_SH",
                WaitResourceKind::DataLock => unreachable!(),
            }
            .into(),
        ),
        elapsed_ms: Some("1000".into()),
        owners: vec![ActivityWaitOwner::Unknown { native_id: "-5".into(), reason: UnknownWaitOwner::UntrackedLatch }],
        resources: None,
    });
    PageWaitObservation {
        source: culprit_core::source_id::PAGE_WAIT_TARGETS.into(),
        index_operations: None,
        observed_from: "2020-01-01T12:00:00.250000000Z".into(),
        observed_until: "2020-01-01T12:00:00.750000000Z".into(),
        request,
        task_wait: None,
        before_check: Some(PageWaitRecheck {
            observed_at: "2020-01-01T12:00:00.300000000Z".into(),
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        }),
        after_check: Some(PageWaitRecheck {
            observed_at: "2020-01-01T12:00:00.700000000Z".into(),
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        }),
        statement_identity: Some(
            NativeIdentity::new(format!("native-sql-{session}")).qualified("statement_start_offset", "0"),
        ),
        plan_identity: Some(NativeIdentity::new(format!("native-plan-{session}"))),
        resource,
        observed_address: Some(PageWaitAddress { database_id: 5, file_id: 1, page_id: page }),
        resolution: PageWaitResolution::ObservedAtBothBounds,
        target: Some(Box::new(MappedPageTarget {
            scope: PageTargetScope::RequestDatabase,
            identity_basis: PageTargetIdentityBasis::LocalDatabaseFileEpoch,
            database: NativeIdentity::new("5").qualified("database_guid", "native-database-guid"),
            file: NativeIdentity::new("native-file-guid").qualified("create_lsn", "00000001:00000001:0001"),
            header: PageHeaderFacts {
                database_id: 5,
                file_id: 1,
                page_id: page,
                header_version: Some(1),
                page_type: Some(2),
                level: Some(0),
                object_id: Some("128".into()),
                index_id: Some("1".into()),
                partition_id: Some("9007199254740993".into()),
                allocation_unit_id: Some("72057594000000001".into()),
                lsn: Some("00000001:00000020:0001".into()),
                next_file_id: Some(1),
                next_page_id: Some(page + 1),
            },
            kind: PageWaitTargetKind::IndexLeaf,
            object: Some(PageWaitObject {
                native: NativeIdentity::new("128").qualified("index_id", "1"),
                schema: "dbo".into(),
                name: "orders".into(),
                index_name: Some("orders_id".into()),
            }),
        })),
    }
}

fn record(observation: PageWaitObservation) -> TelemetryRecord {
    assert!(observation.is_valid());
    TelemetryRecord::Operational(OperationalObservation {
        source: observation.source.clone(),
        key: observation.key().unwrap(),
        fact: OperationalFact::PageWait(Box::new(observation)),
    })
}
