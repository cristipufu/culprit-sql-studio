use super::*;
use crate::{
    activity::{
        memory::tests::{columns, context, reader_with_identity, row},
        tests::details,
    },
    resources::test_support::{FakeRead, replace},
    value::Val,
};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::CurrentRequestMemory },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    }
}
fn source(final_context: crate::value::Rec) -> MemorySource<FakeRead> {
    let other = replace(
        &row(),
        &[
            ("session_id", Val::Int(43)),
            ("grant_session_id", Val::Int(43)),
            ("grant_granted_without_offset", Val::Null),
            ("grant_granted_memory_kb", Val::Null),
            ("grant_wait_time_ms", Val::Int(30_000)),
        ],
    );
    let reader = reader_with_identity(
        vec![
            Ok(vec![context()]),
            Ok(columns()),
            Ok(vec![row(), other]),
            Ok(vec![details(42, "SELECT * FROM t WHERE x=42"), details(43, "SELECT * FROM t WHERE x=43")]),
            Ok(vec![final_context]),
        ],
        IdentityDetail::AzureDatabase,
    );
    MemorySource::from_reader(reader, &request()).unwrap()
}

#[tokio::test]
async fn explicit_request_memory_pages_export_the_same_typed_participants_and_guarded_sql_as_scans() {
    let mut source = source(context());
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.dataset, SnapshotDataset::CurrentRequestMemory);
    assert_eq!(snapshot.records, 2);
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Collected);
    assert_eq!(source.query_count(), 4);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 4);
    let mut cursor = None;
    let mut states = Vec::new();
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(records.len(), 1);
        for record in records {
            let exported = ExportedRecord::new(
                &description,
                &CaptureIdentity { observation_id: "memory-capture".into(), revision: None },
                record,
            )
            .unwrap();
            let TelemetryRecord::Operational(row) = exported.record else { panic!() };
            let OperationalFact::ExecutionGrant(grant) = row.fact else { panic!() };
            assert_eq!(grant.request.as_ref().unwrap().client.as_ref().unwrap().application.as_deref(), Some("app"));
            assert!(grant.is_valid());
            states.push(grant.state);
        }
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(states, [ExecutionGrantState::Granted, ExecutionGrantState::Waiting]);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 5);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 5);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn changed_physical_identity_and_wrong_cursors_cannot_verify_or_reopen_a_capture() {
    let mut changed = source(replace(&context(), &[("physical_incarnation", Val::Text("new-physical".into()))]));
    changed.describe().await.unwrap();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { next_cursor, .. } =
            changed.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(changed.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(changed.close().await, SourceShutdown::Quiescent);
    let mut wrong = source(context());
    wrong.describe().await.unwrap();
    assert!(wrong.read_page(TelemetryStream::OperationalSamples, Some("1")).await.is_err());
    assert!(wrong.finish().await.is_err());
    assert_eq!(wrong.query_count(), 4);
    assert_eq!(wrong.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn non_azure_database_profiles_are_rejected_before_grants_without_falling_back_to_instance_reads() {
    let not_azure = replace(&context(), &[("edition", Val::Int(8)), ("major", Val::Int(12))]);
    let reader = reader_with_identity(vec![Ok(vec![not_azure])], IdentityDetail::AzureDatabase);
    let mut source = MemorySource::from_reader(reader, &request()).unwrap();
    assert_eq!(source.describe().await.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
    assert_eq!(source.query_count(), 1);
    assert!(source.describe().await.is_err());
    assert_eq!(source.query_count(), 1);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
