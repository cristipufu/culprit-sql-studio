//! Real COPY is fixture-only work; collection uses the ordinary read-only focused source and native owner.
use super::{Factory, owned_fixture, retained_activity};
use culprit_collector::capture::{CollectionCancellation, collect_once};
use culprit_core::{ConnectionSpec, Provider, telemetry::*};
use culprit_read::admission::SourceGate;

mod fixture;
use fixture::{COPY_BYTES, COPY_ROWS, Fixture};

fn request(original: &retained_activity::Original) -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::CurrentRequestWork { focus: Box::new(original.focus.clone()) },
        budget: crate::progress::current_work_budget(&CollectionBudget::default()),
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL14+ on the existing nonce-owned loopback fixture; select native_tests::copy::"]
async fn genuine_copy_stdin_retains_exact_original_native_scope_request_and_progress() {
    Fixture::run("observed", |fixture| {
        Box::pin(async move {
            fixture.prepare().await;
            let original =
                retained_activity::original(&fixture.specification, fixture.pid, &fixture.cancellation).await;
            assert!(original.focus.is_current_request_work_focus());
            assert_eq!(original.focus.original.scope, ActivityScopeBasis::SessionDatabase);
            let point = owned_fixture::capture(&fixture.specification, request(&original), &fixture.cancellation).await;
            let source = point.source();
            let SourceObservation::CurrentRequestWork { profile, snapshot, result } = &source.observation else {
                panic!()
            };
            assert_eq!(*profile, CurrentRequestWorkProfile::PostgresCopy);
            assert_eq!(result.binding, FocusedRequestBinding::Matched);
            assert_eq!(result.membership, FocusedRequestMembership::Observed);
            assert_eq!(result.focus.as_deref(), Some(&original.focus));
            assert_eq!(snapshot.records, 1);
            assert_eq!(point.records.len(), 1);
            let TelemetryRecord::Operational(row) = &point.records[0].record else { panic!() };
            let OperationalFact::CurrentRequestWork(work) = &row.fact else { panic!() };
            assert_eq!(work.actor, original.focus.original.key);
            assert_eq!(work.statement_identity, original.focus.statement_identity);
            assert!(work.fits_source_scope(source));
            let CurrentRequestWorkNative::PostgresCopy { progress } = &work.native else { panic!() };
            assert_eq!(progress.command, PostgresCopyCommand::From);
            assert_eq!(progress.io_type, PostgresCopyIo::Pipe);
            assert_eq!(progress.relation_oid, fixture.relation_oid);
            assert_ne!(progress.database_oid, "0");
            assert_eq!(progress.bytes_total.as_deref(), Some("0"));
            let bytes = progress.bytes_processed.as_deref().unwrap().parse::<u64>().unwrap();
            let tuples = progress.tuples_processed.as_deref().unwrap().parse::<u64>().unwrap();
            assert!((1..=COPY_BYTES).contains(&bytes));
            assert!((1..=COPY_ROWS).contains(&tuples));
            assert_eq!(progress.tuples_excluded.as_deref(), Some("0"));
            assert!(matches!(progress.tuples_skipped.as_deref(), None | Some("0")));
            assert_eq!(point.manifest.source_queries, 8, "one setup and seven original-owner reads");
            assert_eq!(
                crate::telemetry_recipes()
                    .into_iter()
                    .find(|recipe| recipe.id == "postgres.current-request-work")
                    .unwrap()
                    .cost,
                RecipeCostQualification::Unqualified
            );
            fixture.release_and_complete().await;
            let absent =
                owned_fixture::capture(&fixture.specification, request(&original), &fixture.cancellation).await;
            let SourceObservation::CurrentRequestWork { profile, snapshot, result } = &absent.source().observation
            else {
                panic!()
            };
            assert_eq!(*profile, CurrentRequestWorkProfile::PostgresCopy);
            assert_eq!(result.membership, FocusedRequestMembership::NotObserved);
            assert_eq!(result.binding, FocusedRequestBinding::ParticipantChanged);
            assert_eq!(result.focus.as_deref(), Some(&original.focus));
            assert_eq!(snapshot.records, 0);
            assert!(absent.records.is_empty());
            assert!(!snapshot.sources[0].complete_scope);
        })
    })
    .await;
}

async fn incomplete(
    specification: &ConnectionSpec,
    request: CollectionRequest,
    cancel: &CollectionCancellation,
) -> CollectionManifest {
    let factory = Factory(crate::PostgresProvider::new(specification).unwrap());
    let directory = tempfile::tempdir().unwrap();
    let manifest =
        collect_once(&SourceGate::default(), &factory, request, &directory.path().join("copy"), cancel).await.unwrap();
    assert_eq!(factory.0.source_shutdown(), SourceShutdown::Quiescent);
    for file in &manifest.files {
        assert_eq!((file.rows, file.bytes), (0, 0), "an incomplete read cannot expose provisional COPY rows");
        assert_eq!(
            std::fs::metadata(directory.path().join("copy").join(&file.file)).unwrap().len(),
            0,
            "the declared stream artifact must contain no provisional COPY bytes"
        );
    }
    if let Some(metrics) = &manifest.read_metrics {
        owned_fixture::assert_settled(metrics, manifest.source_queries);
    }
    manifest
}

#[tokio::test]
#[ignore = "requires PostgreSQL14+ on the existing nonce-owned loopback fixture; select native_tests::copy::"]
async fn caller_budget_cannot_publish_copy_progress_without_its_final_native_owner_check() {
    Fixture::run("limits", |fixture| {
        Box::pin(async move {
            fixture.prepare().await;
            let original =
                retained_activity::original(&fixture.specification, fixture.pid, &fixture.cancellation).await;
            let mut limited = request(&original);
            limited.budget.max_queries = 7;
            let manifest = incomplete(&fixture.specification, limited, &fixture.cancellation).await;
            assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::QueryLimit));
            assert_eq!(manifest.source_queries, 7);
            assert!(manifest.description.is_none(), "a provisional COPY row cannot escape a missing final owner check");
            fixture.release_and_complete().await;
        })
    })
    .await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL14+ on the existing nonce-owned loopback fixture; select native_tests::copy::"]
async fn cancelled_caller_starts_no_copy_source_work_and_keeps_the_owned_writer_independent() {
    Fixture::run("cancelled", |fixture| {
        Box::pin(async move {
            fixture.prepare().await;
            let original =
                retained_activity::original(&fixture.specification, fixture.pid, &fixture.cancellation).await;
            let cancellation = CollectionCancellation::default();
            cancellation.cancel();
            let manifest = incomplete(&fixture.specification, request(&original), &cancellation).await;
            assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::Cancelled));
            assert_eq!(manifest.source_queries, 0);
            fixture.release_and_complete().await;
        })
    })
    .await;
}
