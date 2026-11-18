//! A historical maintenance phase and a later blocking closure keep distinct receipts and owner authority.
use super::*;
use crate::read::NativeRead;
use culprit_collector::{
    capture::{CollectionCancellation, SourceFactory, collect},
    outbox::Outbox,
};
use culprit_core::telemetry::ActivityPolicy;
use culprit_read::admission::SourceGate;

const INDEX_SQL: &str = "CREATE INDEX CONCURRENTLY ix_orders ON orders(id)";
const NEW_OWNER_BACKEND: &str = "2026-11-15T09:59:00.000001Z";

struct ProgressFactory(std::sync::Mutex<Option<NativeReader>>);
#[async_trait::async_trait]
impl SourceFactory for ProgressFactory {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        Ok(crate::progress::fixture_source(self.0.lock().unwrap().take().unwrap(), request))
    }
}
struct ActivityFactory(std::sync::Mutex<Option<NativeReader>>);
#[async_trait::async_trait]
impl SourceFactory for ActivityFactory {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        Ok(crate::telemetry::activity::fixture_source(self.0.lock().unwrap().take().unwrap(), request))
    }
}

struct Captured {
    source: CollectionDescription,
    rows: Vec<ExportedRecord>,
    references: Vec<DiagnosticEvidenceRef>,
}

fn outbox() -> (tempfile::TempDir, Outbox) {
    let directory = tempfile::tempdir().unwrap();
    let outbox = Outbox::create(&directory.path().join("outbox"), OutboxLimits::default()).unwrap();
    (directory, outbox)
}

async fn collected(outbox: &mut Outbox, factory: &dyn SourceFactory, request: CollectionRequest) -> Captured {
    let mut lease = outbox.reserve("maintenance", &request).await.unwrap();
    let identity = lease.identity().clone();
    let output = lease.directory().to_owned();
    let gate = SourceGate::default();
    let manifest = collect(
        &gate,
        factory,
        request.clone(),
        &output,
        identity.clone(),
        &CollectionCancellation::default(),
        Some(&mut lease),
    )
    .await
    .unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    assert!(!gate.is_closed(), "the same native owner acknowledged shutdown");
    lease.finish().await.unwrap();
    assert_eq!(outbox.verify(&identity.observation_id).await.unwrap().capture, identity);
    let source = manifest.description.unwrap();
    assert!(source.matches_request(&request));
    let manifest_sha256 = digest(&std::fs::read(output.join("manifest.json")).unwrap());
    let mut rows = Vec::new();
    let mut references = Vec::new();
    for file in manifest.files {
        for line in std::fs::read_to_string(output.join(file.file)).unwrap().lines() {
            let row: ExportedRecord = serde_json::from_str(line).unwrap();
            assert_eq!(row.record.stream(), file.stream);
            assert_eq!(row, ExportedRecord::new(&source, &row.capture, row.record.clone()).unwrap());
            references
                .push(DiagnosticEvidenceRef::from_export(&source, &manifest_sha256, &row, rows.len() as u32).unwrap());
            rows.push(row);
        }
    }
    Captured { source, rows, references }
}

fn maintenance_actor(lock_wait: bool) -> Row {
    fixture::row(&[
        ("observed_at", Some(AT)),
        ("pid", Some("42")),
        ("backend_start", Some(BACKEND)),
        ("query_start", Some(REQUEST)),
        ("database_oid", Some("16384")),
        ("database", Some("shop")),
        ("state", Some("active")),
        ("elapsed_ms", Some("60000")),
        ("wait_event_type", Some(if lock_wait { "Lock" } else { "IO" })),
        ("wait_event", Some(if lock_wait { "virtualxid" } else { "DataFileRead" })),
    ])
}

async fn original(outbox: &mut Outbox, lock_wait: bool) -> Captured {
    original_with_identity(outbox, lock_wait, Ok(vec![fixture::member("127.0.0.1")])).await
}

async fn original_with_identity(outbox: &mut Outbox, lock_wait: bool, member: Result<Vec<Row>>) -> Captured {
    let reader = NativeReader {
        replies: [
            Ok(vec![fixture::metadata_with("f", PROCESS)]),
            member,
            Ok(vec![maintenance_actor(lock_wait)]),
            Ok(vec![fixture::metadata_with("f", PROCESS)]),
            Ok(vec![fixture::member("127.0.0.1")]),
        ]
        .into(),
        ..Default::default()
    };
    collected(
        outbox,
        &ActivityFactory(std::sync::Mutex::new(Some(reader))),
        CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::Activity,
            budget: crate::telemetry::activity::bounded_budget(&CollectionBudget::default()),
        },
    )
    .await
}

fn participant(capture: &Captured) -> ActivityRequestFocus {
    let focus = participant_candidate(capture);
    assert!(focus.matches_parent(&capture.source, &focus.original, &focus.parent));
    focus
}

fn participant_candidate(capture: &Captured) -> ActivityRequestFocus {
    let index = capture
        .rows
        .iter()
        .position(|row| matches!(&row.record, TelemetryRecord::Activity(row) if row.key.session.id == "42"))
        .unwrap();
    let TelemetryRecord::Activity(row) = &capture.rows[index].record else { panic!() };
    let frame = capture.source.observation.activity().unwrap();
    ActivityRequestFocus {
        parent: capture.references[index].clone(),
        target: capture.source.target.clone(),
        partition: capture.source.partition.clone(),
        database_writable: frame.database_writable,
        source: capture.source.source.clone(),
        epoch: frame.epoch.clone().unwrap(),
        original_observed_at: frame.at.clone(),
        original: row.as_ref().clone(),
        statement_identity: row.statement_identity.clone().unwrap(),
    }
}

async fn maintenance(outbox: &mut Outbox, focus: &ActivityRequestFocus) -> Captured {
    let native = fixture::native_progress(&[
        ("command", Some("CREATE INDEX CONCURRENTLY")),
        ("phase", Some("waiting for writers before build")),
        ("index_oid", Some("16421")),
        ("blocks_total", None),
        ("blocks_done", None),
        ("tuples_total", None),
        ("tuples_done", None),
        ("lockers_total", Some("1")),
        ("lockers_done", Some("0")),
        ("current_locker_pid", Some("43")),
    ]);
    let mut reader = fixture::progress_reader(180_000, 0, Some(native));
    reader.replies[2] = Ok(vec![maintenance_actor(true)]);
    reader.replies[4] = Ok(vec![maintenance_actor(true)]);
    collected(
        outbox,
        &ProgressFactory(std::sync::Mutex::new(Some(reader))),
        CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::MaintenanceProgress { focus: Box::new(focus.clone()) },
            budget: crate::progress::bounded_budget(&CollectionBudget::default()),
        },
    )
    .await
}

fn native_progress(captured: &Captured) -> &MaintenanceProgressObservation {
    let [row] = captured.rows.as_slice() else { panic!("one actual native progress record") };
    let TelemetryRecord::Operational(row) = &row.record else { panic!() };
    let OperationalFact::MaintenanceProgress(point) = &row.fact else { panic!() };
    point
}

fn current_owner(pid: &str, backend: &str) -> Row {
    fixture::row(&[
        ("observed_at", Some(AT)),
        ("pid", Some(pid)),
        ("backend_start", Some(backend)),
        ("query_start", Some("2026-11-15T09:59:30Z")),
        ("transaction_start", Some("2026-11-15T09:59:30Z")),
        ("database_oid", Some("16384")),
        ("database", Some("shop")),
        ("state", Some("idle in transaction")),
        ("blocking_waiters", Some("[42]")),
    ])
}
fn statement() -> Row {
    fixture::row(&[
        ("pid", Some("42")),
        ("query", Some(INDEX_SQL)),
        ("query_bytes", Some(&INDEX_SQL.len().to_string())),
    ])
}
fn blocking_reader(owner_pid: &str, owner_backend: &str) -> NativeReader {
    let mut reader = scripted();
    reader.replies[2] = Ok(vec![maintenance_actor(true)]);
    reader.replies[3] = Ok(vec![edge(&format!("[{owner_pid}]"))]);
    reader.replies[4] = Ok(vec![current_owner(owner_pid, owner_backend)]);
    reader.replies[5] = Ok(vec![fixture::row(&[
        ("pid", Some("42")),
        ("mode", Some("ShareLock")),
        ("locktype", Some("virtualxid")),
        ("elapsed_ms", Some("1")),
        ("object", None),
    ])]);
    reader.replies[6] = Ok(vec![statement()]);
    reader.replies[7] = Ok(vec![maintenance_actor(true)]);
    reader
}
async fn blocking(outbox: &mut Outbox, focus: ActivityRequestFocus, owner: &str, backend: &str) -> Captured {
    collected(outbox, &Factory(std::sync::Mutex::new(Some(blocking_reader(owner, backend)))), request(focus)).await
}
fn activity_rows(captured: &Captured) -> Vec<ActivityFrameRow<'_>> {
    captured
        .rows
        .iter()
        .zip(&captured.references)
        .map(|(row, reference)| {
            let TelemetryRecord::Activity(observation) = &row.record else { panic!() };
            ActivityFrameRow { observation, reference: Some(reference) }
        })
        .collect()
}

#[tokio::test]
async fn original_native_maintenance_lock_wait_already_plans_the_existing_blocking_question() {
    let (_directory, mut outbox) = outbox();
    let original = original(&mut outbox, true).await;
    let focus = participant(&original);
    let update = observe_diagnostic_activity(
        &original.source,
        &focus.parent.capture_ref(),
        &[DiagnosticActivityRow { reference: &focus.parent, observation: &focus.original }],
        None,
        &ActivityPolicy::default(),
        AT,
    )
    .unwrap();
    let episodes = update
        .checkpoint
        .episodes
        .iter()
        .filter(|episode| matches!(episode.signal, DiagnosticSignal::LockWait { .. }))
        .collect::<Vec<_>>();
    assert_eq!(episodes.len(), 1);
    let planned = update.focuses.iter().find(|(id, _)| *id == episodes[0].id).unwrap();
    assert_eq!(planned.1, DiagnosticFocus::Blocking { focus: Box::new(focus.clone()) });
    let progress = maintenance(&mut outbox, &focus).await;
    let SourceObservation::MaintenanceProgress { result, .. } = &progress.source.observation else { panic!() };
    assert_eq!(result.focus.as_deref(), Some(&focus));
    assert_eq!(result.binding, FocusedRequestBinding::Matched);
    assert_eq!(native_progress(&progress).actor, focus.original.key);
    assert_eq!(
        native_progress(&progress).postgres_index_wait().unwrap().phase,
        PostgresIndexWaitPhase::WritersBeforeBuild
    );
    assert_ne!(progress.references[0].capture, focus.parent.capture);
    assert!(matches!(focus.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) }));
}

#[tokio::test]
async fn a_fresh_native_blocker_different_from_the_historical_locker_does_not_rewrite_the_progress_receipt() {
    let (_directory, mut outbox) = outbox();
    let original = original(&mut outbox, true).await;
    let focus = participant(&original);
    let progress = maintenance(&mut outbox, &focus).await;
    let historical = (progress.rows[0].clone(), progress.references[0].clone());
    let current = blocking(&mut outbox, focus.clone(), "44", NEW_OWNER_BACKEND).await;
    let SourceObservation::FocusedBlocking { result, .. } = &current.source.observation else { panic!() };
    assert_eq!(result.focus, focus);
    assert_eq!(result.binding, FocusedRequestBinding::Matched);
    assert_eq!(result.request_end, BlockingRequestState::SameStatement);
    let rows = activity_rows(&current);
    let frame = ActivityFrameEvidence::new(&current.source, &rows, true).unwrap();
    assert!(result.accepts_frame(&frame));
    assert!(result.has_complete_owner_context(&frame), "the original native acquisition already qualified its owner");
    let report = culprit_checks::focused_blocking::explain(&current.source, &rows, true).unwrap();
    assert_eq!(report.original, focus.parent);
    assert_eq!(report.binding, FocusedRequestBinding::Matched);
    assert_eq!(report.context.partition, original.source.partition);
    assert_eq!(report.native_rows, current.rows.len() as u32);
    assert!(report.blocking.as_ref().is_some_and(|graph| graph.chains.iter().any(|chain| chain.head == "44")));
    let current_rows = rows.iter().map(|row| row.observation.clone()).collect::<Vec<_>>();
    let edges = ActivityEvidence::blocking_observations(&current_rows).unwrap();
    assert_eq!(edges.len(), 1);
    assert!(
        matches!(&edges[0].owner, ActivityWaitOwner::Session { id, generation: Some(generation) } if id == "44" && generation == NEW_OWNER_BACKEND)
    );
    assert_eq!(native_progress(&progress).postgres_index_wait().unwrap().unresolved_locker_pid.as_deref(), Some("43"));
    assert_eq!((progress.rows[0].clone(), progress.references[0].clone()), historical);
}

#[tokio::test]
async fn a_sealed_original_with_denied_member_identity_cannot_borrow_the_later_capture_authority() {
    let (_directory, mut outbox) = outbox();
    let original = original_with_identity(
        &mut outbox,
        true,
        Err(Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source"))),
    )
    .await;
    let focus = participant_candidate(&original);
    assert!(matches!(focus.partition, SourcePartition::InstanceLocal { member: None, epoch: Some(_) }));
    assert_eq!(original.source.observation.activity().unwrap().coverage.state, culprit_core::SourceState::Partial);
    assert!(!focus.is_valid());
    let error = outbox.reserve("maintenance", &request(focus)).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
    assert!(matches!(original.source.partition, SourcePartition::InstanceLocal { member: None, .. }));
}

#[tokio::test]
async fn an_explicit_new_qualified_parent_gives_only_the_fresh_owner_a_native_generation() {
    let (_directory, mut outbox) = outbox();
    let original = original(&mut outbox, true).await;
    let logical = participant(&original);
    let qualified = blocking(&mut outbox, logical.clone(), "44", BACKEND).await;
    let focus = participant(&qualified);
    assert_ne!(focus.parent, logical.parent);
    assert!(matches!(logical.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) }));
    assert!(matches!(focus.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) }));
    let progress = maintenance(&mut outbox, &focus).await;
    let SourceObservation::MaintenanceProgress { result, .. } = &progress.source.observation else { panic!() };
    assert_eq!(result.binding, FocusedRequestBinding::Matched);
    let current = blocking(&mut outbox, focus, "43", NEW_OWNER_BACKEND).await;
    let SourceObservation::FocusedBlocking { result, .. } = &current.source.observation else { panic!() };
    let rows = activity_rows(&current);
    let frame = ActivityFrameEvidence::new(&current.source, &rows, true).unwrap();
    assert!(result.has_complete_owner_context(&frame));
    let owner = rows.iter().find(|row| row.observation.key.session.id == "43").unwrap().observation;
    assert_eq!(owner.key.session.generation, NEW_OWNER_BACKEND);
    assert!(owner.key.request.is_none() && owner.statement_identity.is_none());
    assert!(matches!(owner.statement, DefinitionPayload::Unavailable { .. }));
    let wait = native_progress(&progress).postgres_index_wait().unwrap();
    assert_eq!(wait.unresolved_locker_pid.as_deref(), Some("43"));
    assert_eq!(serde_json::to_value(wait).unwrap().as_object().unwrap().len(), 2);
}

#[tokio::test]
async fn a_known_native_phase_cannot_create_a_blocking_focus_when_original_activity_did_not_report_a_lock_wait() {
    let (_directory, mut outbox) = outbox();
    let original = original(&mut outbox, false).await;
    let focus = participant(&original);
    let progress = maintenance(&mut outbox, &focus).await;
    assert!(native_progress(&progress).postgres_index_wait().is_some());
    assert!(focus.is_valid());
    assert!(!focus.is_blocking_focus());
    assert_eq!(CollectionStop::from_error(&request(focus).validate().unwrap_err()), CollectionStop::InvalidPage);
}

#[tokio::test]
async fn the_current_request_can_leave_the_lock_wait_without_borrowing_the_historical_phase_or_locker() {
    let (_directory, mut outbox) = outbox();
    let original = original(&mut outbox, true).await;
    let focus = participant(&original);
    let progress = maintenance(&mut outbox, &focus).await;
    let mut reader = NativeReader {
        replies: [
            Ok(vec![fixture::metadata_with("f", PROCESS)]),
            Ok(vec![fixture::member("127.0.0.1")]),
            Ok(vec![maintenance_actor(false)]),
            Ok(vec![statement()]),
            Ok(vec![maintenance_actor(false)]),
            Ok(vec![fixture::metadata_with("f", PROCESS)]),
            Ok(vec![fixture::member("127.0.0.1")]),
        ]
        .into(),
        ..Default::default()
    };
    let current = capture(&mut reader, "fixture.invalid", 9, &request(focus)).await.unwrap();
    let SourceObservation::FocusedBlocking { result, .. } = &current.description.observation else { panic!() };
    assert_eq!(result.membership, FocusedRequestMembership::Observed);
    assert_eq!(result.request_end, BlockingRequestState::SameStatement);
    assert_eq!(result.closure.status, BlockingClosureStatus::NotRead);
    assert_eq!(current.records.len(), 1);
    assert!(reader.calls.iter().all(|sql| !sql.contains("pg_blocking_pids") && !sql.contains("pg_catalog.pg_locks")));
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(native_progress(&progress).postgres_index_wait().unwrap().unresolved_locker_pid.as_deref(), Some("43"));
}

#[tokio::test]
async fn changed_backend_or_query_generation_after_progress_never_dispatches_the_blocker_reader() {
    let (_directory, mut outbox) = outbox();
    let original = original(&mut outbox, true).await;
    let qualified = blocking(&mut outbox, participant(&original), "44", BACKEND).await;
    let focus = participant(&qualified);
    let progress = maintenance(&mut outbox, &focus).await;
    for changed in [fixture::actor("2026-11-15T08:00:01Z", REQUEST), fixture::actor(BACKEND, "2026-11-15T09:00:01Z")] {
        let mut reader = NativeReader {
            replies: [
                Ok(vec![fixture::metadata_with("f", PROCESS)]),
                Ok(vec![fixture::member("127.0.0.1")]),
                Ok(vec![changed]),
                Ok(vec![fixture::metadata_with("f", PROCESS)]),
                Ok(vec![fixture::member("127.0.0.1")]),
            ]
            .into(),
            ..Default::default()
        };
        let current = capture(&mut reader, "fixture.invalid", 9, &request(focus.clone())).await.unwrap();
        let SourceObservation::FocusedBlocking { result, .. } = &current.description.observation else { panic!() };
        assert_eq!(result.binding, FocusedRequestBinding::ParticipantChanged);
        assert_eq!(result.membership, FocusedRequestMembership::NotObserved);
        assert!(current.records.is_empty());
        assert!(reader.calls.iter().all(|sql| !sql.contains("pg_blocking_pids") && !sql.contains("AS query,")));
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert_eq!(
            native_progress(&progress).postgres_index_wait().unwrap().unresolved_locker_pid.as_deref(),
            Some("43")
        );
    }
}

#[tokio::test]
async fn independent_blocker_membership_and_source_guards_still_control_admission_after_a_progress_receipt() {
    let (_directory, mut outbox) = outbox();
    let original = original(&mut outbox, true).await;
    let qualified = blocking(&mut outbox, participant(&original), "43", BACKEND).await;
    let focus = participant(&qualified);
    let progress = maintenance(&mut outbox, &focus).await;
    for failure in ["membership", "member", "role", "process", "cancel", "budget"] {
        let mut reader = blocking_reader("43", NEW_OWNER_BACKEND);
        match failure {
            "membership" => reader.replies[4] = Ok(vec![fixture::row(&[("observed_at", Some(AT)), ("pid", None)])]),
            "member" => reader.replies[9] = Ok(vec![fixture::member("127.0.0.2")]),
            "role" => reader.replies[8] = Ok(vec![fixture::metadata_with("t", PROCESS)]),
            "process" => reader.replies[8] = Ok(vec![fixture::metadata_with("f", "2026-11-14T01:00:00Z")]),
            "cancel" => reader.replies[4] = Err(CollectionStop::Cancelled.error()),
            _ => reader.query_limit = Some(4),
        }
        let result = capture(&mut reader, "fixture.invalid", 9, &request(focus.clone())).await;
        if failure == "membership" {
            let current = result.unwrap();
            let TelemetryRecord::Activity(waiter) = &current.records[0] else { panic!() };
            assert!(
                matches!(&waiter.wait.as_ref().unwrap().owners[0], ActivityWaitOwner::Session { id, generation: None } if id == "43")
            );
            assert_eq!(
                current.records.len(),
                1,
                "an earlier progress PID cannot fill missing current membership proof"
            );
        } else {
            let expected = match failure {
                "cancel" => CollectionStop::Cancelled,
                "budget" => CollectionStop::QueryLimit,
                _ => CollectionStop::SourceChanged,
            };
            assert_eq!(CollectionStop::from_error(&result.err().unwrap()), expected, "{failure}");
        }
        assert!(reader.calls.len() <= 10);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert_eq!(
            native_progress(&progress).postgres_index_wait().unwrap().unresolved_locker_pid.as_deref(),
            Some("43")
        );
    }
}
