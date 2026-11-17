use super::*;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::sync::atomic::{AtomicBool, Ordering};

fn body(files: &Files, capture: &CaptureIdentity) -> std::path::PathBuf {
    files.directory(&capture.observation_id).join("operations.ndjson")
}
fn activity_body(files: &Files, capture: &CaptureIdentity) -> std::path::PathBuf {
    files.directory(&capture.observation_id).join("activity.ndjson")
}

/// The authorizer runs when check_receipt prepares its journal read, after all native artifact bytes were verified.
fn rewrite_after_verification(
    files: &mut Files,
    path: std::path::PathBuf,
    source: &CollectionDescription,
    change: fn(&mut TelemetryRecord),
) -> (Arc<AtomicBool>, ExportedRecord) {
    let original = std::fs::read_to_string(&path).unwrap();
    assert_eq!(original.lines().count(), 1, "this race control has one real original row at ordinal zero");
    let mut changed: ExportedRecord = serde_json::from_str(original.trim()).unwrap();
    change(&mut changed.record);
    assert!(changed.record.is_valid(), "the replacement is meaningful canonical data, not malformed noise");
    let canonical = ExportedRecord::new(source, &changed.capture, changed.record.clone()).unwrap();
    assert_eq!(canonical.payload_sha256().unwrap(), changed.payload_sha256().unwrap());
    let replacement = format!("{}\n", serde_json::to_string(&changed).unwrap());
    assert_ne!(original, replacement);
    assert_eq!(original.len(), replacement.len(), "file length cannot expose this replacement");
    let fired = Arc::new(AtomicBool::new(false));
    let once = fired.clone();
    files
        .journal
        .conn
        .authorizer(Some(move |context: AuthContext<'_>| {
            if matches!(context.action, AuthAction::Read { table_name: "captures", column_name: "manifest_sha" })
                && !once.swap(true, Ordering::SeqCst)
            {
                std::fs::write(&path, &replacement).unwrap();
            }
            Authorization::Allow
        }))
        .unwrap();
    (fired, changed)
}
fn stop_rewriting(files: &mut Files) {
    files.journal.conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>).unwrap();
}
fn change_elapsed(record: &mut TelemetryRecord) {
    match record {
        TelemetryRecord::Activity(row) => row.elapsed_ms = Some("90001".into()),
        TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page), .. }) => {
            page.request.elapsed_ms = Some("90001".into());
        }
        _ => panic!("elapsed counter fixture"),
    }
}
fn assert_original(actual: &DiagnosticInputRow, expected: &DiagnosticInputRow, changed: &ExportedRecord) {
    assert_eq!(actual.reference, expected.reference);
    assert_eq!(actual.reference.row_ordinal, 0);
    assert_eq!(actual.row.payload_sha256().unwrap(), expected.row.payload_sha256().unwrap());
    assert_ne!(actual.row.payload_sha256().unwrap(), changed.payload_sha256().unwrap());
    assert_eq!(actual.reference.payload_sha256, expected.row.payload_sha256().unwrap());
}

#[test]
fn activity_inputs_and_direct_references_use_verified_originals_when_same_length_bytes_change_after_the_hash() {
    for partial in [false, true] {
        for direct_reference in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut files = Files::create(&root.path().join("outbox"), Default::default()).unwrap();
            let capture = baseline(&mut files, vec![activity("42")], !partial, 0);
            let original = files.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
            assert_eq!(original.rows.len(), 1);
            original.rows[0]
                .reference
                .validate_canonical_record(&original.source, &original.rows[0].row.record)
                .unwrap();
            let path = activity_body(&files, &capture);
            let (fired, changed) = rewrite_after_verification(&mut files, path, &original.source, change_elapsed);
            if direct_reference {
                let (source, row) = files.diagnostic_reference(&original.rows[0].reference).unwrap();
                assert_eq!(source, original.source);
                assert_eq!(row.payload_sha256().unwrap(), original.rows[0].row.payload_sha256().unwrap());
            } else {
                let replay = files.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
                assert_eq!(replay.source, original.source);
                assert_original(&replay.rows[0], &original.rows[0], &changed);
            }
            assert!(fired.load(Ordering::SeqCst), "the actual post-hash journal boundary must be exercised");
            stop_rewriting(&mut files);
            let error = files.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap_err();
            assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Outbox(OutboxProblem::Integrity)));
            assert_eq!(files.journal.capture(&capture.observation_id).unwrap().state, OutboxCaptureState::Quarantined);
        }
    }
}

#[test]
fn a_shared_owner_never_resigns_a_changed_counter_under_the_original_manifest_receipt() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), Default::default()).unwrap();
    let fixture = crate::analytical::test_support::shared_investigation_fixture();
    let CollectionTarget::Shared(resource) = fixture.owner_source.target.clone() else { panic!() };
    let capture = shared::seal(
        &mut files,
        CollectionRequest {
            subject: CollectionSubject::Shared { resource },
            selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
            budget: Default::default(),
        },
        fixture.owner_source,
        vec![fixture.owner_record.record],
        None,
    );
    let (reference, source, original) = files.diagnostic_owner(&capture).unwrap().unwrap();
    assert_eq!(reference.row_ordinal, 0);
    let path = body(&files, &capture);
    let (fired, changed) = rewrite_after_verification(&mut files, path, &source, |record| {
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(set), .. }) =
            record
        else {
            panic!()
        };
        set.counters[0].value = MeasurementValue::Integer("13".into());
    });
    let (actual_reference, actual_source, actual) = files.diagnostic_owner(&capture).unwrap().unwrap();
    assert!(fired.load(Ordering::SeqCst));
    assert_eq!(actual_source, source);
    assert_eq!(actual_reference, reference);
    assert_eq!(actual.payload_sha256().unwrap(), original.payload_sha256().unwrap());
    assert_ne!(actual.payload_sha256().unwrap(), changed.payload_sha256().unwrap());
    stop_rewriting(&mut files);
    assert!(files.diagnostic_owner(&capture).is_err());
    assert_eq!(files.journal.capture(&capture.observation_id).unwrap().state, OutboxCaptureState::Quarantined);
}

fn focused(files: &mut Files, request_state: PageWaitRequestState, with_row: bool) -> CaptureIdentity {
    let capture = baseline(files, vec![focus::page_activity()], true, 0);
    let original = files.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    let focus = focus::focus(&original);
    let result = ActivityWaitFocusResult {
        focus: focus.clone(),
        observed_from: "2026-11-14T08:00:02Z".into(),
        observed_until: "2026-11-14T08:00:03Z".into(),
        source: FocusSourceState::Matched,
        request: request_state,
        wait: if with_row { PageWaitRelation::OriginalObserved } else { PageWaitRelation::Unqualified },
    };
    let page = PageWaitObservation {
        task_wait: None,
        source: "native-page-point".into(),
        observed_from: result.observed_from.clone(),
        observed_until: result.observed_until.clone(),
        request: focus.participant.original.clone(),
        before_check: None,
        after_check: None,
        statement_identity: None,
        plan_identity: None,
        resource: focus.resource.clone(),
        observed_address: None,
        resolution: PageWaitResolution::Unavailable,
        target: None,
        index_operations: None,
    };
    let rows = if with_row {
        vec![TelemetryRecord::Operational(OperationalObservation {
            source: page.source.clone(),
            key: page.key().unwrap(),
            fact: OperationalFact::PageWait(Box::new(page)),
        })]
    } else {
        vec![]
    };
    let source = CollectionDescription {
        target: original.source.target,
        partition: original.source.partition,
        source: "focused-page-reader".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedPageWait {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitTargets,
                observed_from: result.observed_from.clone(),
                observed_until: result.observed_until.clone(),
                records: rows.len() as u32,
                sources: vec![StateSourceCoverage::observed("native-page-point".into(), rows.len() as u32, !with_row)],
            },
            result: Box::new(result),
        },
    };
    shared::seal(
        files,
        CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::FocusedPageWait { focus: Box::new(focus) },
            budget: Default::default(),
        },
        source,
        rows,
        None,
    )
}

#[test]
fn a_focused_partial_point_keeps_its_original_body_and_parent_after_the_post_hash_race() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), Default::default()).unwrap();
    let capture = focused(&mut files, PageWaitRequestState::SameStatement, true);
    let original = files.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    let path = body(&files, &capture);
    let (fired, changed) = rewrite_after_verification(&mut files, path, &original.source, change_elapsed);
    let replay = files.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    assert!(fired.load(Ordering::SeqCst));
    assert_eq!(replay.source, original.source);
    assert_eq!(replay.source.observation.snapshot().unwrap().sources[0].state, culprit_core::SourceState::Partial);
    assert_original(&replay.rows[0], &original.rows[0], &changed);
    stop_rewriting(&mut files);
    assert!(files.diagnostic_reference(&original.rows[0].reference).is_err());
    assert_eq!(files.journal.capture(&capture.observation_id).unwrap().state, OutboxCaptureState::Quarantined);
}

#[test]
fn missing_changed_and_empty_focus_results_remain_exact_empty_points_not_original_rows() {
    for request in [PageWaitRequestState::Missing, PageWaitRequestState::Changed, PageWaitRequestState::Unverified] {
        let root = tempfile::tempdir().unwrap();
        let mut files = Files::create(&root.path().join("outbox"), Default::default()).unwrap();
        let capture = focused(&mut files, request, false);
        let input = files.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
        assert!(input.rows.is_empty());
        let SourceObservation::FocusedPageWait { result, snapshot } = &input.source.observation else { panic!() };
        assert_eq!(snapshot.records, 0);
        assert_eq!(result.request, request);
        assert_eq!(result.wait, PageWaitRelation::Unqualified);
        assert_ne!(result.focus.participant.parent.capture, capture);
    }
}

#[test]
fn restored_originals_stay_on_their_checksummed_journal_path_without_reopening_local_artifacts() {
    let root = tempfile::tempdir().unwrap();
    let mut original = Files::create(&root.path().join("original"), Default::default()).unwrap();
    let capture = baseline(&mut original, vec![activity("42")], false, 0);
    let input = original.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    let store = digest(b"controlled-recovery-owner");
    let mut restored = Files::create_remote(&root.path().join("cold"), Default::default(), &store).unwrap();
    restored
        .restore_diagnostic_input(RestoredInput {
            store_id: store,
            parent: input.parent.clone(),
            source: input.source.clone(),
            investigation: input.investigation.clone(),
            connection: input.connection.clone(),
            rows: input.rows.iter().map(|row| row.row.clone()).collect(),
        })
        .unwrap();
    let cold = restored.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    assert_eq!(cold.rows[0].reference, input.rows[0].reference);
    assert!(!restored.directory(&capture.observation_id).exists());
    let recorded: String = restored
        .journal
        .conn
        .query_row(
            "SELECT restored_json FROM diagnostic_inputs WHERE observation_id=?1",
            [&capture.observation_id],
            |row| row.get(0),
        )
        .unwrap();
    let changed = recorded.replace("90000", "90001");
    assert_ne!(recorded, changed);
    assert_eq!(recorded.len(), changed.len());
    restored
        .journal
        .conn
        .execute(
            "UPDATE diagnostic_inputs SET restored_json=?1 WHERE observation_id=?2",
            params![changed, capture.observation_id],
        )
        .unwrap();
    assert!(restored.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).is_err());
    assert!(restored.diagnostic_reference(&input.rows[0].reference).is_err());
}

#[test]
fn unconfirmed_native_work_cannot_publish_a_buffer_and_generic_cached_runtime_cannot_enter_the_question_queue() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), Default::default()).unwrap();
    let capture = baseline(&mut files, vec![activity("42")], true, 0);
    files
        .journal
        .conn
        .execute(
            "UPDATE captures SET source_work=?1 WHERE observation_id=?2",
            params![
                super::super::super::journal::encode(&SourceWorkState::Unconfirmed).unwrap(),
                capture.observation_id
            ],
        )
        .unwrap();
    assert!(files.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).is_err());
    let state = files.journal.capture(&capture.observation_id).unwrap();
    assert_eq!(state.source_work, SourceWorkState::Unconfirmed);
    assert_eq!(state.state, OutboxCaptureState::Quarantined);

    let mut unrelated = Files::create(&root.path().join("unrelated"), Default::default()).unwrap();
    let (source, rows) = crate::analytical::test_support::cached_runtime_evidence_cases().pop().unwrap();
    let capture = shared::seal(
        &mut unrelated,
        CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::CachedRuntime },
            budget: Default::default(),
        },
        source,
        rows,
        None,
    );
    assert!(unrelated.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).unwrap().is_none());
    assert!(unrelated.diagnostic_owner(&capture).unwrap().is_none());
    assert_eq!(unrelated.journal.capture(&capture.observation_id).unwrap().state, OutboxCaptureState::Ready);
}
