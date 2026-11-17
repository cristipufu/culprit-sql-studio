use super::shared::seal;
use super::*;
use crate::analytical::test_support::{PageTaskHolderCase, page_task_holder_fixture};

fn request(
    activity: DiagnosticCaptureRef,
    original_context: NativeReadContext,
    original_requests: Vec<ActivityRequestSelector>,
) -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::PageWaitTasks {
            activity,
            original_context: Box::new(original_context),
            original_requests,
        },
        budget: Default::default(),
    }
}
fn original(files: &mut Files, missing: Option<&str>) -> DiagnosticInput {
    let mut fixture = page_task_holder_fixture(PageTaskHolderCase::QualifiedBoth);
    if let Some(missing) = missing {
        let SourceObservation::Activity(native) = &mut fixture.activity_source.observation else { panic!() };
        if missing == "role" {
            native.database_writable = None;
        } else {
            let SourcePartition::InstanceLocal { member, epoch } = &mut fixture.activity_source.partition else {
                panic!()
            };
            *member = None;
            *epoch = None;
            native.epoch = None;
            native.coverage =
                StateSourceCoverage::observed(fixture.activity_source.source.clone(), native.observations, false);
        }
    }
    let capture = seal(
        files,
        CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::Activity,
            budget: Default::default(),
        },
        fixture.activity_source,
        fixture.activity_rows,
        None,
    );
    files.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap()
}

#[test]
fn paired_population_authenticates_actual_original_manifest_and_native_scope_before_reserving_or_dispatching() {
    for missing in ["manifest", "role", "owner"] {
        let root = tempfile::tempdir().unwrap();
        let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
        let input = original(&mut files, (missing != "manifest").then_some(missing));
        let mut reference = input.parent.clone();
        if missing == "manifest" {
            reference.manifest_sha256 = digest(b"not-original");
        }
        let before = files.journal.ids().unwrap();
        let error = match files.reserve(
            "fixture",
            &request(
                reference,
                input.activity_frame().unwrap().context(),
                input.activity_frame().unwrap().page_wait_task_requests().unwrap(),
            ),
        ) {
            Err(error) => error,
            Ok(_) => panic!("unproven original cannot start paired native work"),
        };
        assert_eq!(files.journal.ids().unwrap(), before, "{missing}: no native reservation was installed");
        if missing != "manifest" {
            assert_eq!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)));
        }
    }
}

#[test]
fn paired_population_pins_its_original_before_native_start_and_keeps_the_pin_for_unconfirmed_work_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut files = Files::create(&path, OutboxLimits::default()).unwrap();
    let input = original(&mut files, None);
    let request = request(
        input.parent.clone(),
        input.activity_frame().unwrap().context(),
        input.activity_frame().unwrap().page_wait_task_requests().unwrap(),
    );
    let reservation = files.reserve("fixture", &request).unwrap();
    let capture = reservation.entry.identity.clone();
    let owner = format!("focus:{}", capture.observation_id);
    let pinned: Vec<String> = {
        let mut query =
            files.journal.conn.prepare("SELECT observation_id FROM diagnostic_pins WHERE owner=?1").unwrap();
        query.query_map([&owner], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    };
    assert_eq!(pinned.as_slice(), std::slice::from_ref(&input.parent.capture.observation_id));
    assert_eq!(reservation.entry.source_work, SourceWorkState::NotStarted);
    let writer = ObservationFiles::create(&reservation.directory, request, capture.clone(), None).unwrap();
    files.journal.source_work(&reservation.entry, SourceWorkState::Active).unwrap();
    files.journal.source_work(&reservation.entry, SourceWorkState::Unconfirmed).unwrap();
    drop(writer);
    drop(reservation);
    drop(files);
    let mut reopened = Files::open(&path).unwrap();
    reopened.recover().unwrap();
    assert!(super::focus::pin(&reopened, &owner));
    assert_eq!(reopened.journal.capture(&capture.observation_id).unwrap().source_work, SourceWorkState::Unconfirmed);
}

#[test]
fn holder_native_reservation_pins_both_exact_original_captures_and_rejects_a_substituted_page_receipt() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    let input = original(&mut files, None);
    let mut fixture = page_task_holder_fixture(PageTaskHolderCase::QualifiedBoth);
    let SourceObservation::PageWaitTargets { original_activity, .. } = &mut fixture.page_source.observation else {
        panic!()
    };
    *original_activity = Some(input.parent.clone());
    let page = seal(
        &mut files,
        request(
            input.parent.clone(),
            input.activity_frame().unwrap().context(),
            input.activity_frame().unwrap().page_wait_task_requests().unwrap(),
        ),
        fixture.page_source,
        fixture.page_rows,
        None,
    );
    let page = files.diagnostic_input(&page.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(point), .. }) =
        &page.rows[0].row.record
    else {
        panic!()
    };
    let DiagnosticFocusPlan::Ready(question) =
        plan_page_task_holder_focus(&page.source, point, &page.rows[0].reference, &input.activity_frame().unwrap())
            .unwrap()
    else {
        panic!()
    };
    let DiagnosticFocus::PageTaskHolders { focus } = *question else { panic!() };
    let mut forged = focus.clone();
    forged.page.payload_sha256 = digest(b"substituted-page");
    let before = files.journal.ids().unwrap();
    assert!(
        files
            .reserve(
                "fixture",
                &CollectionRequest {
                    subject: CollectionSubject::Database,
                    selection: CollectionSelection::PageTaskHolders { focus: forged },
                    budget: Default::default()
                }
            )
            .is_err()
    );
    assert_eq!(files.journal.ids().unwrap(), before);
    let reservation = files
        .reserve(
            "fixture",
            &CollectionRequest {
                subject: CollectionSubject::Database,
                selection: CollectionSelection::PageTaskHolders { focus },
                budget: Default::default(),
            },
        )
        .unwrap();
    let owner = format!("focus:{}", reservation.entry.identity.observation_id);
    let pinned: std::collections::BTreeSet<String> = {
        let mut query =
            files.journal.conn.prepare("SELECT observation_id FROM diagnostic_pins WHERE owner=?1").unwrap();
        query.query_map([&owner], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    };
    assert_eq!(
        pinned,
        std::collections::BTreeSet::from([input.parent.capture.observation_id, page.parent.capture.observation_id])
    );
}

#[test]
fn paired_population_cannot_substitute_original_context_even_when_every_field_is_structurally_valid() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    let input = original(&mut files, None);
    let context = input.activity_frame().unwrap().context();
    for field in ["target", "member", "epoch", "role", "from", "until"] {
        let mut changed = context.clone();
        match field {
            "target" => {
                let CollectionTarget::Database(database) = &mut changed.target else { panic!() };
                database.database = "foreign-database".into();
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition else {
                    panic!()
                };
                member.native.id = "later-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { panic!() };
                *epoch = Some("later-native-epoch".into());
            }
            "role" => changed.database_writable = Some(false),
            "from" => changed.observed_from = "2020-01-01T00:00:00Z".into(),
            _ => changed.observed_until = "2030-01-01T00:00:00Z".into(),
        }
        let request =
            request(input.parent.clone(), changed, input.activity_frame().unwrap().page_wait_task_requests().unwrap());
        request.validate().unwrap();
        let before = files.journal.ids().unwrap();
        assert!(files.reserve("fixture", &request).is_err(), "{field}");
        assert_eq!(files.journal.ids().unwrap(), before, "{field}: no reservation/native owner");
    }
}

#[test]
fn paired_reservation_rejects_omitted_injected_or_changed_original_requests_before_creating_native_work() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    let input = original(&mut files, None);
    let frame = input.activity_frame().unwrap();
    let expected = frame.page_wait_task_requests().unwrap();
    assert!(expected.len() > 1, "the authentic original fixture includes multiple requests");
    for change in ["omitted", "injected", "token", "generation"] {
        let mut requests = expected.clone();
        match change {
            "omitted" => {
                requests.pop();
            }
            "injected" => {
                let mut extra = requests[0].clone();
                extra.key.session.id = "999".into();
                requests.push(extra);
            }
            "token" => requests[0].statement_identity.id = "other-native-statement".into(),
            _ => requests[0].key.session.generation = "other-native-generation".into(),
        }
        requests.sort_by(|left, right| left.key.cmp(&right.key));
        assert!(ActivityRequestSelector::is_canonical(&requests), "{change}: structurally valid is not authenticated");
        let before = files.journal.ids().unwrap();
        let request = request(input.parent.clone(), frame.context(), requests);
        assert!(files.reserve("fixture", &request).is_err(), "{change}");
        assert_eq!(files.journal.ids().unwrap(), before, "{change}: no native reservation");
    }
    let accepted = request(input.parent.clone(), frame.context(), expected);
    assert!(files.reserve("fixture", &accepted).is_ok());
}

#[test]
fn an_oversized_complete_original_roster_is_refused_before_reservation_without_trimming_or_new_limits() {
    let root = tempfile::tempdir().unwrap();
    let mut files = Files::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    let fixture = page_task_holder_fixture(PageTaskHolderCase::QualifiedBoth);
    let mut source = fixture.activity_source;
    let TelemetryRecord::Activity(actor) = &fixture.activity_rows[0] else { panic!() };
    // One ordinary admitted page can retain this many actual requests. The controller's revision-input
    // limit is not an Activity-frame row limit and its smaller fixture did not exercise request-byte capacity.
    let original_rows = CollectionBudget::default().page_rows as usize;
    assert!(original_rows <= MAX_ACTIVITY_FRAME_OBSERVATIONS);
    let rows = (1..=original_rows)
        .map(|index| {
            let mut actor = actor.as_ref().clone();
            actor.key.session.id = (1000 + index).to_string();
            actor.statement_identity = Some(
                NativeIdentity::new(format!("{index:0128X}"))
                    .qualified("native_object", "sys.dm_exec_requests.sql_handle")
                    .qualified("database_id", "7")
                    .qualified("statement_start_offset", "0")
                    .qualified("statement_end_offset", "-1"),
            );
            TelemetryRecord::from(actor)
        })
        .collect::<Vec<_>>();
    let SourceObservation::Activity(native) = &mut source.observation else { panic!() };
    native.observations = rows.len() as u32;
    native.native_rows = rows.len() as u32;
    native.coverage.records = rows.len() as u32;
    let parent = seal(
        &mut files,
        CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::Activity,
            budget: Default::default(),
        },
        source,
        rows,
        None,
    );
    let input = files.diagnostic_input(&parent.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    let frame = input.activity_frame().unwrap();
    let requests = frame.page_wait_task_requests().unwrap();
    assert_eq!(requests.len(), original_rows);
    assert!(ActivityRequestSelector::is_canonical(&requests));
    let request = request(input.parent.clone(), frame.context(), requests);
    request.validate().unwrap();
    assert!(serde_json::to_vec(&request).unwrap().len() > crate::outbox::MAX_REQUEST_BYTES);
    let before = files.journal.ids().unwrap();
    let error = match files.reserve("fixture", &request) {
        Err(error) => error,
        Ok(_) => panic!("oversized full roster cannot create native source work"),
    };
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Outbox(OutboxProblem::Capacity)));
    assert_eq!(files.journal.ids().unwrap(), before);
    assert_eq!(
        files.diagnostic_input(&parent.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap().rows.len(),
        original_rows
    );
}
