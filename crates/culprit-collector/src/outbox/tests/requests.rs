//! Original native focus evidence is retained verbatim; its journal bytes also remain part of the quota.
use super::*;
use crate::analytical::test_support::{
    InvestigationFixture, compiler_admission_evidence, compiler_admission_investigation_fixture,
    focused_request_memory_evidence, request_memory_investigation_fixture,
};
mod diagnostic_inputs;

const ORIGINAL_SQL_BYTES: usize = 16 * 1024;

fn statement(bytes: usize) -> DefinitionPayload {
    let prefix = "SELECT [value] FROM [dbo].[native_request] WHERE [value] IS NOT NULL;";
    assert!(bytes >= prefix.len());
    DefinitionPayload::sql(&format!("{prefix}{}", " ".repeat(bytes - prefix.len())), culprit_core::Engine::Mssql)
}

fn request_for(fixture: &InvestigationFixture) -> CollectionRequest {
    CollectionRequest {
        subject: fixture.intent.subject(),
        selection: fixture.intent.selection(),
        budget: fixture.intent.budget.clone(),
    }
}

fn set_original_statement(fixture: &mut InvestigationFixture, sql: DefinitionPayload) {
    let TelemetryRecord::Activity(activity) = &mut fixture.baseline_record.record else { panic!() };
    activity.statement = sql.clone();
    fixture.intent.focus.participant_mut().original.statement = sql;
}

fn seal(
    files: &mut Files,
    request: CollectionRequest,
    source: CollectionDescription,
    records: Vec<TelemetryRecord>,
) -> CaptureIdentity {
    let reservation = files.reserve("fixture", &request).unwrap();
    let identity = reservation.entry.identity.clone();
    complete_reservation(files, reservation, request, source, records);
    identity
}

fn complete_reservation(
    files: &mut Files,
    reservation: Reservation,
    request: CollectionRequest,
    source: CollectionDescription,
    records: Vec<TelemetryRecord>,
) {
    files.journal.source_work(&reservation.entry, SourceWorkState::Active).unwrap();
    let mut writer =
        ObservationFiles::create(&reservation.directory, request, reservation.entry.identity.clone(), None).unwrap();
    writer.describe(source).unwrap();
    writer.begin_page(0).unwrap();
    assert!(writer.write_page(0, &records).unwrap().is_none());
    writer.seal(0).unwrap();
    writer.confirm_source(SourceCompletion::Verified).unwrap();
    assert_eq!(writer.finish(0).unwrap().status, CollectionStatus::Complete);
    files.journal.source_work(&reservation.entry, SourceWorkState::Quiescent).unwrap();
    files.finish(reservation).unwrap();
}

fn qualified_request(files: &mut Files, mut fixture: InvestigationFixture) -> CollectionRequest {
    set_original_statement(&mut fixture, statement(ORIGINAL_SQL_BYTES));
    let baseline = seal(
        files,
        CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::Activity,
            budget: CollectionBudget::default(),
        },
        fixture.baseline_source.clone(),
        vec![fixture.baseline_record.record.clone()],
    );
    let input = files.diagnostic_input(&baseline.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    let focus = fixture.intent.focus.participant_mut();
    focus.parent = input.rows[0].reference.clone();
    let request = request_for(&fixture);
    request.validate().unwrap();
    let focus = request.focus().unwrap();
    let TelemetryRecord::Activity(activity) = &input.rows[0].row.record else { panic!() };
    assert!(focus.matches_parent(&input.source, activity, &input.rows[0].reference));
    request
}

fn focused_source(
    request: &CollectionRequest,
    mut source: CollectionDescription,
    mut records: Vec<TelemetryRecord>,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let focus = request.focus().unwrap();
    match &mut source.observation {
        SourceObservation::FocusedCompilationAdmission { result, .. } => result.focus = focus.clone(),
        SourceObservation::FocusedRequestMemory { result, .. } => result.focus = focus.clone(),
        _ => panic!(),
    }
    for record in &mut records {
        let TelemetryRecord::Operational(row) = record else { panic!() };
        match &mut row.fact {
            OperationalFact::CompilerWaiter(waiter) => waiter.request = focus.original.clone(),
            OperationalFact::ExecutionGrant(grant) => grant.request = Some(focus.original.clone()),
            OperationalFact::CounterSet(_) => (),
            _ => panic!(),
        }
    }
    assert!(source.matches_request(request));
    (source, records)
}

#[test]
fn valid_long_compiler_and_memory_focuses_keep_exact_original_receipts_through_commit_and_reopen() {
    for (fixture, evidence) in [
        (compiler_admission_investigation_fixture(), compiler_admission_evidence()),
        (request_memory_investigation_fixture(), focused_request_memory_evidence()),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outbox");
        let mut files = Files::create(&path, OutboxLimits::default()).unwrap();
        let request = qualified_request(&mut files, fixture);
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.len() > 4096 && encoded.len() <= MAX_REQUEST_BYTES);
        let DefinitionPayload::Available { text, .. } = &request.focus().unwrap().original.statement else { panic!() };
        assert_eq!(text.len(), ORIGINAL_SQL_BYTES);
        let (source, records) = focused_source(&request, evidence.0, evidence.1);
        let reservation = files.reserve("fixture", &request).unwrap();
        let id = reservation.entry.identity.observation_id.clone();
        assert_eq!(
            reservation.entry.charged_bytes,
            u64::from(request.budget.total_bytes) + CAPTURE_ALLOWANCE + encoded.len() as u64
        );
        complete_reservation(&mut files, reservation, request.clone(), source, records);
        let artifacts = measure(&files.directory(&id)).unwrap();
        assert_eq!(
            files.journal.capture(&id).unwrap().charged_bytes,
            artifacts + CAPTURE_ACCOUNTING + encoded.len() as u64
        );
        let stored: String = files
            .journal
            .conn
            .query_row("SELECT request_json FROM captures WHERE observation_id=?1", [&id], |row| row.get(0))
            .unwrap();
        assert_eq!(stored, encoded);
        drop(files);
        let mut files = Files::open(&path).unwrap();
        files.recover().unwrap();
        let capture = files.journal.capture(&id).unwrap();
        assert_eq!(capture.state, OutboxCaptureState::Ready);
        assert_eq!(capture.request, request);
        assert_eq!(capture.charged_bytes, artifacts + CAPTURE_ACCOUNTING + encoded.len() as u64);
        assert_eq!(files.verify(&id).unwrap().request, request);
    }
}

#[test]
fn incomplete_and_interrupted_focuses_retain_their_request_charge_without_native_or_artifact_work() {
    for interrupted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outbox");
        let mut files = Files::create(&path, OutboxLimits::default()).unwrap();
        let request = qualified_request(&mut files, compiler_admission_investigation_fixture());
        let bytes = serde_json::to_vec(&request).unwrap().len() as u64;
        let reservation = files.reserve("fixture", &request).unwrap();
        let id = reservation.entry.identity.observation_id.clone();
        assert!(!reservation.directory.exists());
        if !interrupted {
            files.finish(reservation).unwrap();
        }
        drop(files);
        let mut files = Files::open(&path).unwrap();
        let recovery = files.recover().unwrap();
        assert_eq!(recovery.interrupted, u32::from(interrupted));
        let capture = files.journal.capture(&id).unwrap();
        assert_eq!(capture.request, request);
        assert_eq!(capture.source_work, SourceWorkState::NotStarted);
        assert_eq!(capture.charged_bytes, CAPTURE_ACCOUNTING + bytes);
        assert_eq!(
            capture.state,
            if interrupted { OutboxCaptureState::Interrupted } else { OutboxCaptureState::Incomplete }
        );
        let status = files.status(10, None).unwrap();
        assert_eq!(
            status.charged_bytes,
            status.entries.iter().map(|entry| entry.charged_bytes).sum::<u64>()
                + investigations::journal::charged_bytes(&files.journal.conn).unwrap()
        );
    }
}

fn journal_with_quota(root: &tempfile::TempDir, bytes: u64) -> journal::Journal {
    journal::Journal::create(
        &root.path().join("journal.sqlite3"),
        &"a".repeat(32),
        OutboxLimits { max_bytes: bytes, max_captures: 2 },
    )
    .unwrap()
}

fn assert_no_allocation(journal: &journal::Journal) {
    let (captures, epoch, sequence): (u32, i64, i64) = journal
        .conn
        .query_row("SELECT (SELECT count(*) FROM captures),owner_epoch,sequence FROM metadata", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!((captures, epoch, sequence), (0, 0, 0));
}

#[test]
fn reservation_quota_includes_exact_serialized_request_bytes_before_allocating_a_revision() {
    let mut fixture = compiler_admission_investigation_fixture();
    set_original_statement(&mut fixture, statement(ORIGINAL_SQL_BYTES));
    let request = request_for(&fixture);
    request.validate().unwrap();
    let bytes = serde_json::to_vec(&request).unwrap().len() as u64;
    let charge = u64::from(request.budget.total_bytes) + CAPTURE_ALLOWANCE + bytes;
    for admitted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut journal = journal_with_quota(&root, charge - u64::from(!admitted));
        let mut epoch = None;
        match journal.reserve("fixture", &request, &mut epoch) {
            Ok(capture) => {
                assert!(admitted);
                assert_eq!(capture.charged_bytes, charge);
                assert_eq!(journal.status(10, None).unwrap().charged_bytes, charge);
            }
            Err(error) => {
                assert!(!admitted);
                assert_eq!(kind(&error), OutboxProblem::Capacity);
                assert_eq!(epoch, None);
                assert_no_allocation(&journal);
            }
        }
    }
}

fn request_with_size(bytes: usize) -> CollectionRequest {
    let mut fixture = compiler_admission_investigation_fixture();
    set_original_statement(&mut fixture, statement(ORIGINAL_SQL_BYTES));
    let initial = serde_json::to_vec(&request_for(&fixture)).unwrap().len();
    set_original_statement(&mut fixture, statement(ORIGINAL_SQL_BYTES + bytes - initial));
    let request = request_for(&fixture);
    assert_eq!(serde_json::to_vec(&request).unwrap().len(), bytes);
    request.validate().unwrap();
    request
}

#[test]
fn one_common_request_byte_limit_accepts_the_boundary_and_refuses_excess_without_allocation() {
    for bytes in [MAX_REQUEST_BYTES, MAX_REQUEST_BYTES + 1] {
        let root = tempfile::tempdir().unwrap();
        let mut journal = journal_with_quota(&root, OutboxLimits::default().max_bytes);
        let mut epoch = None;
        let request = request_with_size(bytes);
        match journal.reserve("fixture", &request, &mut epoch) {
            Ok(capture) => {
                assert_eq!(bytes, MAX_REQUEST_BYTES);
                assert_eq!(journal.capture(&capture.identity.observation_id).unwrap().request, request);
            }
            Err(error) => {
                assert_eq!(bytes, MAX_REQUEST_BYTES + 1);
                assert_eq!(kind(&error), OutboxProblem::Capacity);
                assert!(!error.to_string().contains("native_request"));
                assert_eq!(epoch, None);
                assert_no_allocation(&journal);
            }
        }
    }
}

#[test]
fn persistent_request_constraint_counts_utf8_bytes_rather_than_sqlite_characters() {
    let root = tempfile::tempdir().unwrap();
    let mut journal = journal_with_quota(&root, OutboxLimits::default().max_bytes);
    let capture = journal.reserve("fixture", &request(), &mut None).unwrap();
    let mut request = request_with_size(MAX_REQUEST_BYTES);
    let CollectionSelection::FocusedCompilationAdmission { focus } = &mut request.selection else { panic!() };
    let DefinitionPayload::Available { text, sha256, .. } = &mut focus.original.statement else { panic!() };
    *text = text.replacen("[value]", "[valúe]", 1);
    *sha256 = digest(text.as_bytes());
    request.validate().unwrap();
    let json = serde_json::to_string(&request).unwrap();
    assert_eq!(json.len(), MAX_REQUEST_BYTES + 1);
    assert!(json.chars().count() <= MAX_REQUEST_BYTES);
    let error = journal
        .conn
        .execute(
            "UPDATE captures SET request_json=?1 WHERE observation_id=?2",
            rusqlite::params![json, capture.identity.observation_id],
        )
        .unwrap_err();
    assert_eq!(error.sqlite_error_code(), Some(rusqlite::ErrorCode::ConstraintViolation));
    assert_eq!(journal.capture(&capture.identity.observation_id).unwrap().request, super::request());
}
