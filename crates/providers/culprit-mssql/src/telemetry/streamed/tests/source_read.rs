//! Protocol conformance only: these fixtures neither authenticate a durable parent nor qualify production cost.
use super::*;
use crate::telemetry::compilation::QueryCompilation;
mod measurement;

const FIRST: &str = "2020-01-01T08:01:00Z";
const LAST: &str = "2020-01-01T08:01:01Z";

fn request() -> CollectionRequest {
    let original = crate::activity::compiler_admission::tests::focus();
    let context = NativeReadContext {
        target: original.target.clone(),
        partition: original.partition.clone(),
        database_writable: original.database_writable,
        observed_from: original.original_observed_at.clone(),
        observed_until: original.original_observed_at.clone(),
    };
    let prerequisite = prerequisite(&original);
    CollectionRequest {
        subject: CollectionSubject::Database,
        budget: CollectionBudget::default(),
        selection: CollectionSelection::SourceRead {
            focus: Box::new(SourceReadFocus {
                basis: Box::new(original.parent),
                target: original.target,
                question: SourceReadQuestion::CompilationHistory,
                prerequisite: SourceReadPrerequisite::CompilationAdmission { report: Box::new(prerequisite) },
            }),
            execution: Box::new(NativeSourceReadScope::from_context(&context).unwrap()),
        },
    }
}
fn prerequisite(original: &ActivityRequestFocus) -> CompilationAdmissionReport {
    let waiter = CompilerWaiterObservation { observed_at: FIRST.into(), request: original.original.clone() };
    let source_name = crate::sources::COMPILATION_GATEWAYS.name.to_owned();
    let native = OperationalObservation {
        source: source_name.clone(),
        key: waiter.key().unwrap(),
        fact: OperationalFact::CompilerWaiter(Box::new(waiter)),
    };
    let source = CollectionDescription {
        target: original.target.clone(),
        partition: original.partition.clone(),
        source: source_name.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedCompilationAdmission {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::CompilerAdmission,
                records: 1,
                observed_from: FIRST.into(),
                observed_until: FIRST.into(),
                sources: vec![StateSourceCoverage::observed(source_name, 1, false)],
            },
            result: Box::new(ActivityCompilationAdmissionFocusResult {
                focus: original.clone(),
                observed_from: FIRST.into(),
                observed_until: FIRST.into(),
                source: FocusSourceState::Matched,
                membership: CompilationAdmissionMembership::Observed,
                gateways: vec![],
            }),
        },
    };
    let capture = CaptureIdentity {
        observation_id: "protocol-only-original-compiler-context".into(),
        revision: Some(CaptureRevision {
            outbox_id: "native-unit-fixture".into(),
            owner_epoch: "1".into(),
            sequence: "2".into(),
        }),
    };
    let row = ExportedRecord::new(&source, &capture, TelemetryRecord::Operational(native)).unwrap();
    CompilationAdmissionReport {
        observation: DiagnosticEvidenceRef::from_export(&source, &digest(b"protocol-context-manifest"), &row, 0)
            .unwrap(),
        gateways: vec![],
    }
}
fn authority(at: &str) -> Rec {
    replace(&crate::activity::memory::tests::context(), &[("observed_at", Val::Text(at.into()))])
}
fn native(before: Rec, after: Rec, shutdown: SourceShutdown) -> StreamingRead {
    let mut native = FakeRead::new(vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![before]),
        Ok(vec![clock(FIRST)]),
        Ok(vec![pressure()]),
        Ok(vec![pressure()]),
        Ok(vec![clock(LAST)]),
        Ok(vec![after]),
    ]);
    native.shutdown = shutdown;
    StreamingRead {
        native,
        batches: vec![batch(crate::query_store::tests::compilation_row(), true)].into(),
        starts: 0,
        pause_batches: false,
    }
}
fn source(before: Rec, after: Rec, shutdown: SourceShutdown) -> StreamedSnapshot<QueryCompilation, StreamingRead> {
    StreamedSnapshot::with_io(
        native(before, after, shutdown),
        &request(),
        "fixture.invalid".into(),
        1433,
        QueryCompilation,
    )
    .unwrap()
}

#[tokio::test]
async fn qualified_source_read_preserves_logical_namespace_and_existing_eight_query_held_lifecycle() {
    let request = request();
    let mut source = source(authority(FIRST), authority(LAST), SourceShutdown::Quiescent);
    assert_eq!(source.query_count(), 0);
    let initial = source.describe().await.unwrap();
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.inner.reader().io.starts, 0);
    assert_eq!(initial.partition, SourcePartition::Target);
    assert!(initial.observation.execution_context().unwrap().has_qualified_local_owner());
    assert!(initial.matches_request(&request));
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(records.len(), 1);
    assert!(next_cursor.is_none());
    let TelemetryRecord::Operational(row) = &records[0] else { panic!() };
    assert!(matches!(row.fact, OperationalFact::MssqlQueryCompilation(_)));
    let completion = source.finish().await.unwrap();
    let final_source = initial.completed(completion).unwrap().unwrap();
    assert!(matches!(final_source.observation, SourceObservation::SourceRead { .. }));
    assert!(final_source.matches_request(&request));
    let data = NativeReadContext::from_snapshot(&final_source).unwrap();
    assert_eq!(data.partition, SourcePartition::Target);
    assert_eq!(data.database_writable, None);
    assert_eq!(source.query_count(), 8);
    assert_eq!(source.inner.reader().io.starts, 1);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn changed_initial_member_database_process_or_updateability_never_reaches_layout_resources_or_body() {
    for (field, value) in [
        ("replica_id", Val::Text("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into())),
        ("physical_incarnation", Val::Text("99999999-1111-2222-3333-444444444444".into())),
        ("incarnation", Val::Text("88888888-1111-2222-3333-444444444444".into())),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("updateability", Val::Null),
        ("local_process_id", Val::Int(12)),
    ] {
        let before = replace(&authority(FIRST), &[(field, value)]);
        let mut source = source(before, authority(LAST), SourceShutdown::Quiescent);
        assert_eq!(
            CollectionStop::from_error(&source.describe().await.unwrap_err()),
            CollectionStop::SourceChanged,
            "{field}"
        );
        assert_eq!(source.query_count(), 2, "{field}: only edition and initial native authority");
        assert_eq!(source.inner.reader().io.starts, 0);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn final_owner_change_withholds_all_provisional_rows_and_preserves_uncertain_shutdown() {
    let after = replace(
        &authority(LAST),
        &[("physical_incarnation", Val::Text("77777777-1111-2222-3333-444444444444".into()))],
    );
    let mut source = source(authority(FIRST), after, SourceShutdown::Unconfirmed);
    source.describe().await.unwrap();
    source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.query_count(), 8);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert!(source.read_page(TelemetryStream::OperationalSamples, None).await.is_err());
}

#[tokio::test]
async fn terminal_initial_failure_has_no_followup_native_read_or_fallback() {
    for stop in [CollectionStop::Cancelled, CollectionStop::TimedOut] {
        let native = FakeRead::new(vec![Ok(vec![Rec::of(&[("edition", Val::Int(5))])]), Err(ReadError::Stopped(stop))]);
        let io = StreamingRead { native, batches: VecDeque::new(), starts: 0, pause_batches: false };
        let mut source =
            StreamedSnapshot::with_io(io, &request(), "fixture.invalid".into(), 1433, QueryCompilation).unwrap();
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), stop);
        assert_eq!(source.query_count(), 2);
        assert_eq!(source.inner.reader().io.starts, 0);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[test]
fn undeclared_recipe_refuses_the_source_question_without_native_construction_work() {
    let io =
        StreamingRead { native: FakeRead::new(Vec::new()), batches: VecDeque::new(), starts: 0, pause_batches: false };
    assert!(StreamedSnapshot::with_io(io, &request(), "fixture.invalid".into(), 1433, Grants).is_err());
}
