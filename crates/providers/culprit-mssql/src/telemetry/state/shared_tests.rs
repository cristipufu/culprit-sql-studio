use super::*;
use crate::{
    instance::test_support::{epoch, metadata},
    resources::test_support::{FakeRead, replace},
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
fn source(final_identity: Rec, shutdown: SourceShutdown) -> StateSource<FakeRead> {
    let replies = vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![super::tests::clock(false, "2020-01-01T06:02:00Z")]),
        Ok(vec![super::tests::space_row()]),
        Ok(vec![crate::memory_clerks::tests::row()]),
        Ok(crate::schedulers::fixture_columns()),
        Ok(vec![crate::schedulers::tests::row()]),
        Ok(vec![crate::performance::tests::row()]),
        Ok(vec![crate::processors::tests::row()]),
        Ok(vec![crate::compiler_gateways::tests::row()]),
        Ok(vec![super::tests::clock(false, "2020-01-01T06:02:01Z")]),
        Ok(vec![final_identity]),
        Ok(vec![epoch(100)]),
    ];
    let mut reader = FakeRead::new(replies);
    reader.shutdown = shutdown;
    StateSource::with_io(reader, &request(), "unused-member-route.invalid".into(), 1433).unwrap()
}

#[tokio::test]
async fn instance_capture_reuses_native_readers_and_never_charges_the_shared_facts_to_a_database() {
    let mut source = source(metadata(), SourceShutdown::Quiescent);
    let description = source.describe().await.unwrap();
    assert!(matches!(description.target, CollectionTarget::Shared(_)));
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.sources.len(), 6);
    assert_eq!(snapshot.records, 8);
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
            _ => panic!("shared native resource family"),
        }
    }
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 13);
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
