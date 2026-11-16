use super::*;
use crate::activity::{
    compiler_admission::tests::{focus, replies, waiter_row},
    memory::tests::{context, reader_with_identity},
};
use crate::resources::test_support::{FakeRead, replace};
use crate::value::Val;

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::FocusedCompilationAdmission { focus: Box::new(focus()) },
        budget: focused_budget(&CollectionBudget::default()),
    }
}
fn source(changed_final: bool) -> CompilerAdmissionSource<FakeRead> {
    let mut replies = replies(Some(waiter_row()), true);
    let last = if changed_final {
        replace(&context(), &[("physical_incarnation", Val::Text("changed-completion-owner".into()))])
    } else {
        context()
    };
    replies.push(Ok(vec![last]));
    CompilerAdmissionSource::from_reader(reader_with_identity(replies, IdentityDetail::AzureDatabase), &request())
        .unwrap()
}

#[tokio::test]
async fn focused_source_binds_exact_question_raw_receipts_and_confirmed_completion() {
    let mut source = source(false);
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request()));
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(records.len(), 2);
    assert!(next_cursor.is_none());
    for record in records {
        ExportedRecord::new(
            &description,
            &CaptureIdentity { observation_id: "synthetic-compiler-focus".into(), revision: None },
            record,
        )
        .unwrap();
    }
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_final_owner_change_cannot_complete_and_a_generic_snapshot_cannot_replace_the_question() {
    let mut source = source(true);
    let description = source.describe().await.unwrap();
    let mut generic = request();
    generic.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::CompilerAdmission };
    assert!(!description.matches_request(&generic));
    source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn an_absent_or_unverified_waiter_never_declares_an_unread_gateway_population_empty() {
    for unverified in [false, true] {
        let row = if unverified {
            replace(&waiter_row(), &[("request_generation", Val::Text("2020-01-01T07:58:01".into()))])
        } else {
            replace(&waiter_row(), &[("wait_type", Val::Text("RESOURCE_SEMAPHORE".into()))])
        };
        let mut values = replies(Some(row), false);
        values.push(Ok(vec![context()]));
        let mut source = CompilerAdmissionSource::from_reader(
            reader_with_identity(values, IdentityDetail::AzureDatabase),
            &request(),
        )
        .unwrap();
        let description = source.describe().await.unwrap();
        let SourceObservation::FocusedCompilationAdmission { snapshot, result } = &description.observation else {
            panic!()
        };
        assert_eq!(
            result.membership,
            if unverified {
                CompilationAdmissionMembership::Unverified
            } else {
                CompilationAdmissionMembership::NotObserved
            }
        );
        assert_eq!(snapshot.records, 0);
        assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Partial);
        assert!(!snapshot.sources[0].complete_scope, "no gateway query ran, so no gateway absence was established");
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
        else {
            panic!()
        };
        assert!(records.is_empty() && next_cursor.is_none());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(
            source.query_count(),
            4,
            "only owner, original request and final owner proofs; no enrichment or gateway query"
        );
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
