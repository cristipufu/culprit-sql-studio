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
    [
        (full, culprit_core::SourceState::Collected),
        (partial, culprit_core::SourceState::Partial),
        (vec![], culprit_core::SourceState::Empty),
        (vec![], culprit_core::SourceState::Unavailable),
    ]
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
        observed_from: "2020-01-01T12:00:00.250000000Z".into(),
        observed_until: "2020-01-01T12:00:00.750000000Z".into(),
        request,
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
