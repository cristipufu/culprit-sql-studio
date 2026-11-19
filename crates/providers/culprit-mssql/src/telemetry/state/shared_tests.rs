use super::*;
use crate::{
    instance::test_support::{epoch, metadata},
    resources::test_support::{FakeRead, instance_pressure_row, replace},
    value::{Rec, Val},
};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Shared {
            resource: SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("fixture-machine").qualified("kind", "sql-server-network-machine"),
                native: NativeIdentity::new("fixture-machine\\NATIVE").qualified("kind", "sql-server-instance"),
            },
        },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
        budget: CollectionBudget { page_rows: 2, ..Default::default() },
    }
}
fn replies(final_identity: Rec) -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![super::tests::clock(false, "2020-01-01T06:02:00Z")]),
        Ok(vec![super::tests::temp_space_row()]),
        Ok(vec![crate::operations::native::test_support::temp_log_layout_row()]),
        Ok(vec![crate::operations::native::test_support::temp_log_row()]),
        Ok(vec![crate::memory_clerks::tests::row()]),
        Ok(vec![replace(&instance_pressure_row(), &[("observed_at", Val::Text("2020-01-01T06:02:00.5Z".into()))])]),
        Ok(crate::schedulers::fixture_columns()),
        Ok(vec![crate::schedulers::tests::row()]),
        Ok(vec![crate::performance::tests::row()]),
        Ok(vec![crate::processors::tests::row()]),
        Ok(vec![crate::compiler_gateways::tests::row()]),
        Ok(vec![super::tests::clock(false, "2020-01-01T06:02:01Z")]),
        Ok(vec![final_identity]),
        Ok(vec![epoch(100)]),
    ]
}
fn source(final_identity: Rec, shutdown: SourceShutdown) -> StateSource<FakeRead> {
    let mut reader = FakeRead::new(replies(final_identity));
    reader.shutdown = shutdown;
    StateSource::with_io(reader, &request(), "unused-member-route.invalid".into(), 1433).unwrap()
}

#[tokio::test]
async fn instance_capture_reuses_native_readers_and_never_charges_the_shared_facts_to_a_database() {
    let mut source = source(metadata(), SourceShutdown::Quiescent);
    let description = source.describe().await.unwrap();
    assert!(matches!(description.target, CollectionTarget::Shared(_)));
    let CollectionTarget::Shared(member) = &description.target else { panic!() };
    let SourcePartition::InstanceLocal { member: Some(owner), epoch: Some(epoch) } = &description.partition else {
        panic!("the already-read native process authority must survive export");
    };
    assert_eq!(owner, member);
    assert!(valid_digest(epoch));
    assert!(description.is_partition_qualified());
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.sources.len(), 8);
    assert_eq!(snapshot.records, 10);
    assert!(snapshot.sources.iter().all(|source| source.state == culprit_core::SourceState::Collected));
    let capture = CaptureIdentity { observation_id: "native-instance".into(), revision: None };
    let mut cursor = None;
    let mut records = vec![];
    loop {
        let CollectionRead::Page { records: page, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        records.extend(page);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    for record in records {
        let row = ExportedRecord::new(&description, &capture, record).unwrap();
        assert!(row.record_id.is_some());
        let TelemetryRecord::Operational(observation) = row.record else { panic!() };
        match observation.fact {
            OperationalFact::Counter(counter) => assert_eq!(counter.scope, culprit_core::MeasurementScope::Server),
            OperationalFact::CounterSet(set) => assert_eq!(set.scope, culprit_core::MeasurementScope::Server),
            OperationalFact::Space(space) => assert_eq!(space.scope, culprit_core::FileScope::Server),
            OperationalFact::MssqlInstanceResources(point) => {
                assert!(point.has_complete_population());
                assert!(point.matches_context(&NativeReadContext::from_snapshot(&description).unwrap()));
            }
            _ => panic!("shared native resource family"),
        }
    }
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 16);
    assert_eq!(source.io.calls.iter().filter(|call| call.sql.contains("sys.dm_os_process_memory")).count(), 1);
    assert!(
        !source.io.calls.iter().any(|call| call.sql.contains("sys.dm_db_resource_stats")),
        "native SQL Server instance statistics do not use Azure database pressure views"
    );
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_wrong_native_instance_stops_before_statistics_and_recycled_processes_cannot_verify_a_capture() {
    let mut wrong = request();
    let CollectionSubject::Shared { resource } = &mut wrong.subject else { panic!() };
    resource.native.id = "another-instance".into();
    let mut source = StateSource::with_io(
        FakeRead::new(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)])]),
        &wrong,
        "fixture.invalid".into(),
        1433,
    )
    .unwrap();
    assert_eq!(CollectionStop::from_error(&source.describe().await.err().unwrap()), CollectionStop::SourceChanged);
    assert_eq!(source.query_count(), 2);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let mut source = self::source(replace(&metadata(), &[("process_id", Val::Int(43))]), SourceShutdown::Quiescent);
    source.describe().await.unwrap();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { next_cursor, .. } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
}

#[tokio::test]
async fn shared_source_preserves_unconfirmed_shutdown_for_the_capture_owner() {
    let mut source = source(metadata(), SourceShutdown::Unconfirmed);
    source.describe().await.unwrap();
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert!(source.read_page(TelemetryStream::OperationalSamples, None).await.is_err());
}

#[tokio::test]
async fn the_emitted_shared_partition_is_the_existing_native_process_generation_without_additional_queries() {
    let mut reader = FakeRead::new(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)])]);
    let native = crate::instance::read(&mut reader).await.unwrap().0;
    let mut source = source(metadata(), SourceShutdown::Quiescent);
    let description = source.describe().await.unwrap();
    assert_eq!(
        description.partition,
        SourcePartition::InstanceLocal {
            member: Some(native.identity.clone()),
            epoch: Some(native.capture_epoch().unwrap()),
        }
    );
    assert_eq!(source.query_count(), 14, "the partition reuses the initial authority already owned by this read");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn unavailable_tempdb_log_keeps_the_independent_data_population_and_does_not_requery() {
    const LOG_LAYOUT_REPLY: usize = 4;
    for (field, value, expected) in [
        ("log_permission", Val::Int(0), culprit_core::ErrorCode::PermissionDenied),
        ("usage_columns", Val::Int(2), culprit_core::ErrorCode::Unsupported),
    ] {
        let mut replies = replies(metadata());
        replies[LOG_LAYOUT_REPLY] =
            Ok(vec![replace(&crate::operations::native::test_support::temp_log_layout_row(), &[(field, value)])]);
        replies.remove(LOG_LAYOUT_REPLY + 1);
        let mut source =
            StateSource::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
        let description = source.describe().await.unwrap();
        let snapshot = description.observation.snapshot().unwrap();
        let log =
            snapshot.sources.iter().find(|source| source.source == culprit_core::source_id::LOG_CAPACITY).unwrap();
        let data =
            snapshot.sources.iter().find(|source| source.source == culprit_core::source_id::TEMP_CAPACITY).unwrap();
        assert_eq!(log.state, culprit_core::SourceState::Unavailable);
        assert_eq!(log.error_code, Some(expected));
        assert_eq!(log.records, 0);
        assert_eq!(data.state, culprit_core::SourceState::Collected);
        assert_eq!(data.records, 1);
        assert_eq!(snapshot.records, 9);
        assert_eq!(
            source.io.calls.iter().filter(|call| call.sql.contains("FROM tempdb.sys.dm_db_log_space_usage")).count(),
            0
        );
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    assert!(
        !point::families(SnapshotDataset::Operational)
            .unwrap()
            .iter()
            .any(|family| { matches!(family, point::Family::Operation(crate::operations::native::Kind::TempLog)) })
    );
}
