use super::*;
use crate::{
    activity::memory::{
        focus::tests::{native_row, original_focus, replies},
        tests::{context, reader_with_identity},
    },
    resources::test_support::{FakeRead, replace},
    value::Val,
};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::FocusedRequestMemory { focus: Box::new(original_focus()) },
        budget: crate::request_memory::bounded_budget(&CollectionBudget::default()),
    }
}
fn source(changed_owner: bool) -> MemorySource<FakeRead> {
    let mut replies = replies(vec![native_row()], true);
    if changed_owner {
        *replies.last_mut().unwrap() =
            Ok(vec![replace(&context(), &[("physical_incarnation", Val::Text("changed-native-generation".into()))])]);
    }
    MemorySource::from_reader(reader_with_identity(replies, IdentityDetail::AzureDatabase), &request()).unwrap()
}

#[tokio::test]
async fn focused_memory_exports_exact_native_participation_once_with_the_original_question_and_final_owner_check() {
    let mut source = source(false);
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request()));
    let SourceObservation::FocusedRequestMemory { result, snapshot } = &description.observation else { panic!() };
    assert_eq!(result.focus, original_focus());
    assert_eq!(snapshot.records, 1);
    assert_eq!(result.membership, RequestMemoryMembership::Observed);
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(records.len(), 1);
    assert!(next_cursor.is_none());
    let row = ExportedRecord::new(
        &description,
        &CaptureIdentity { observation_id: "focused-memory-export".into(), revision: None },
        records[0].clone(),
    )
    .unwrap();
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::ExecutionGrant(grant), .. }) =
        row.record
    else {
        panic!()
    };
    assert!(result.accepts_observation(&grant));
    assert_eq!(grant.requested_kibibytes.as_deref(), Some("9007199254740993"));
    assert!(grant.request.as_ref().unwrap().statement.is_available());
    assert!(
        !result.focus.original.statement.is_available(),
        "later SQL availability never overwrites the authenticated original"
    );
    source.finish().await.unwrap();
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_final_owner_change_cannot_complete_the_focused_source() {
    let mut source = source(true);
    source.describe().await.unwrap();
    source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_bare_snapshot_or_another_native_question_cannot_substitute_for_the_focused_capture() {
    let mut source = source(false);
    let mut description = source.describe().await.unwrap();
    let mut manual = request();
    manual.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::CurrentRequestMemory };
    assert!(!description.matches_request(&manual));
    let SourceObservation::FocusedRequestMemory { snapshot, .. } = description.observation else { panic!() };
    description.observation = SourceObservation::Snapshot(snapshot);
    assert!(!description.matches_request(&request()));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
