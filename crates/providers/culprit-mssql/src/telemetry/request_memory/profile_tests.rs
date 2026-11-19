use super::*;
use crate::{
    activity::{
        memory::tests::{columns, context, row},
        tests::details,
    },
    instance::test_support::{epoch, metadata},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::CurrentRequestMemory },
        budget: CollectionBudget { page_rows: 1, max_snapshot_rows: 64, ..Default::default() },
    }
}

fn source(replies: Vec<Vec<Rec>>) -> MemorySource<FakeRead> {
    let request = request();
    let reader = ActivityReader::new(
        FakeRead::new(replies.into_iter().map(Ok).collect()),
        read_options(&request),
        "fixture.invalid".into(),
        1433,
    );
    MemorySource::from_reader(reader, &request).unwrap()
}

fn azure_replies(native: Rec, final_context: Rec) -> Vec<Vec<Rec>> {
    vec![
        vec![crate::grants::tests::platform()],
        vec![context()],
        columns(),
        vec![native],
        vec![details(42, "SELECT * FROM dbo.orders WHERE id=42")],
        vec![final_context],
    ]
}

async fn grant(source: &mut MemorySource<FakeRead>, description: &CollectionDescription) -> ExecutionGrantObservation {
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(records.len(), 1);
    assert!(next_cursor.is_none());
    let exported = ExportedRecord::new(
        description,
        &CaptureIdentity { observation_id: "memory-native-profile".into(), revision: None },
        records.into_iter().next().unwrap(),
    )
    .unwrap();
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::ExecutionGrant(grant), .. }) =
        exported.record
    else {
        panic!()
    };
    *grant
}

#[tokio::test]
async fn factory_options_admit_boxed_memory_with_its_original_local_owner_role_and_nine_query_bound() {
    for writable in ["READ_WRITE", "READ_ONLY"] {
        let owner = replace(
            &crate::activity::database_context::tests::row(3),
            &[("updateability", Val::Text(writable.into()))],
        );
        let native = replace(&row(), &[("edition", Val::Int(3)), ("updateability", Val::Text(writable.into()))]);
        let mut source = source(vec![
            vec![metadata()],
            vec![epoch(100)],
            vec![owner.clone()],
            columns(),
            vec![native],
            vec![details(42, "SELECT * FROM dbo.orders WHERE id=42")],
            vec![metadata()],
            vec![epoch(100)],
            vec![owner],
        ]);
        let description = source.describe().await.unwrap();
        assert!(description.target.database().unwrap().incarnation.is_none());
        let SourcePartition::InstanceLocal { member: Some(member), epoch: Some(_) } = &description.partition else {
            panic!()
        };
        assert_eq!(member.kind, SharedResourceKind::Instance);
        assert!(description.partition.is_qualified());
        assert_eq!(source.query_count(), 6);
        let grant = grant(&mut source, &description).await;
        assert_eq!(grant.requested_kibibytes.as_deref(), Some("9007199254740993"));
        assert!(grant.request.as_ref().unwrap().statement.is_available());
        assert!(grant.workload_group_tempdb.is_none());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 9);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn factory_options_preserve_azure_logical_and_physical_identity_after_endpoint_discovery() {
    let mut source = source(azure_replies(row(), context()));
    let description = source.describe().await.unwrap();
    assert_eq!(description.target.database().unwrap().incarnation.as_deref(), Some("logical-guid"));
    let SourcePartition::InstanceLocal { member: Some(member), epoch: Some(_) } = &description.partition else {
        panic!()
    };
    assert_eq!(member.kind, SharedResourceKind::Node);
    assert_eq!(member.namespace.id, "logical-guid");
    assert_eq!(member.native.id, "91487244-730d-4b83-8e49-d482241c7288");
    assert!(description.partition.is_qualified());
    assert_eq!(source.query_count(), 5);
    let grant = grant(&mut source, &description).await;
    assert!(grant.workload_group_tempdb.is_none());
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn endpoint_discovery_does_not_bypass_azure_primary_or_known_platform_admission() {
    for role in [Val::Text("READ_ONLY".into()), Val::Null] {
        let mut source =
            source(vec![vec![crate::grants::tests::platform()], vec![replace(&context(), &[("updateability", role)])]]);
        assert_eq!(source.describe().await.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
        assert_eq!(source.query_count(), 2, "no column inventory or grant read follows an unknown/secondary role");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let mut unknown = source(vec![vec![replace(&metadata(), &[("edition", Val::Int(0))])]]);
    assert_eq!(unknown.describe().await.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
    assert_eq!(unknown.query_count(), 1);
    assert_eq!(unknown.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn changed_azure_logical_physical_or_role_identity_refuses_the_original_frame_before_sql_enrichment() {
    for (field, changed) in [
        ("incarnation", "another-logical-database"),
        ("physical_incarnation", "another-physical-database"),
        ("updateability", "READ_ONLY"),
    ] {
        let mut source = source(azure_replies(replace(&row(), &[(field, Val::Text(changed.into()))]), context()));
        let error = source.describe().await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert_eq!(source.query_count(), 4, "{field}: no replacement read or SQL enrichment");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn changed_final_azure_logical_or_physical_owner_cannot_verify_the_frozen_capture() {
    for field in ["incarnation", "physical_incarnation"] {
        let mut source =
            source(azure_replies(row(), replace(&context(), &[(field, Val::Text("another-owner".into()))])));
        let description = source.describe().await.unwrap();
        grant(&mut source, &description).await;
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed, "{field}");
        assert_eq!(source.query_count(), 6);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
