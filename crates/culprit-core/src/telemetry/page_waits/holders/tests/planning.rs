use super::*;
mod incidents;

struct Scenario {
    source: CollectionDescription,
    page_source: CollectionDescription,
    page: PageWaitObservation,
    page_ref: DiagnosticEvidenceRef,
    rows: Vec<ActivityObservation>,
    refs: Vec<DiagnosticEvidenceRef>,
}

impl Scenario {
    fn new() -> Self {
        let (focus, _, value) = fixture();
        let mut page = *value.page.unwrap();
        page.observed_from = focus.task.observed_at.clone();
        page.observed_until = focus.original_observed_until.clone();
        page.task_wait = Some(focus.task.clone());
        page.before_check.as_mut().unwrap().observed_at = "2026-11-13T10:00:00.3Z".into();
        page.after_check.as_mut().unwrap().observed_at = "2026-11-13T10:00:00.7Z".into();
        let mut original = focus.participant.original;
        original.wait.as_mut().unwrap().native_type = Some("CXCONSUMER".into());
        original.wait.as_mut().unwrap().identified_resources.clear();
        page.request = original.clone();
        let source = CollectionDescription {
            target: focus.participant.target.clone(),
            partition: focus.participant.partition.clone(),
            source: focus.participant.source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Activity(ActivitySnapshotDescription {
                epoch: Some(focus.participant.epoch.clone()),
                database_writable: Some(true),
                observed_from: focus.participant.original_observed_at.clone(),
                at: focus.participant.original_observed_at,
                observed_until: "2026-11-13T09:59:59.1Z".into(),
                scope: crate::MeasurementScope::Database,
                population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
                native_rows: 1,
                observations: 1,
                coverage: StateSourceCoverage::collected(focus.participant.source, 1),
            }),
        };
        let page_source = CollectionDescription {
            target: source.target.clone(),
            partition: source.partition.clone(),
            source: page.source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PageWaitTargets {
                database_writable: Some(true),
                original_activity: None,
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageWaitTargets,
                    observed_from: page.observed_from.clone(),
                    observed_until: page.observed_until.clone(),
                    records: 1,
                    sources: vec![StateSourceCoverage::observed(page.source.clone(), 1, false)],
                },
            },
        };
        let rows = vec![original];
        let refs = crate::telemetry::activity_frame::tests::references(&source, &rows);
        let mut value = Self { source, page_source, page, page_ref: focus.page, rows, refs };
        value.rebind_page();
        value
    }

    fn rebind_page(&mut self) {
        self.rebind_page_as("original-task-page");
    }

    fn rebind_page_as(&mut self, observation_id: &str) {
        if let SourceObservation::PageWaitTargets { original_activity, .. } = &mut self.page_source.observation
            && let Some(reference) = self.refs.first()
        {
            *original_activity = Some(reference.capture_ref());
        }
        let capture = CaptureIdentity {
            observation_id: observation_id.into(),
            revision: Some(CaptureRevision {
                outbox_id: "controlled-original-page-fixture".into(),
                owner_epoch: "1".into(),
                sequence: "2".into(),
            }),
        };
        let row = TelemetryRecord::Operational(OperationalObservation {
            source: self.page.source.clone(),
            key: self.page.key().unwrap(),
            fact: OperationalFact::PageWait(Box::new(self.page.clone())),
        });
        let exported = ExportedRecord::new(&self.page_source, &capture, row).unwrap();
        self.page_ref = DiagnosticEvidenceRef::from_export(
            &self.page_source,
            &digest(b"controlled original manifest"),
            &exported,
            0,
        )
        .unwrap();
    }

    fn rebind_frame(&mut self) {
        self.refs = crate::telemetry::activity_frame::tests::references(&self.source, &self.rows);
    }
    fn frame(&self, complete: bool) -> ActivityFrameEvidence<'_> {
        let rows = crate::telemetry::activity_frame::tests::borrowed(&self.rows, &self.refs);
        ActivityFrameEvidence::new(&self.source, &rows, complete).unwrap()
    }
    fn plan(&self, complete: bool) -> Result<DiagnosticFocusPlan> {
        plan_page_task_holder_focus(&self.page_source, &self.page, &self.page_ref, &self.frame(complete))
    }
}

#[test]
fn explicit_task_question_uses_both_original_receipts_without_creating_a_coordinator_wait_or_incident() {
    let scenario = Scenario::new();
    let DiagnosticFocusPlan::Ready(question) = scenario.plan(true).unwrap() else { panic!("qualified original task") };
    let DiagnosticFocus::PageTaskHolders { focus } = &*question else { panic!("native task question") };
    assert_eq!(focus.participant.parent, scenario.refs[0]);
    assert_eq!(focus.page, scenario.page_ref);
    assert_eq!(focus.task, scenario.page.task_wait.clone().unwrap());
    assert_eq!(focus.participant.original, scenario.rows[0]);
    assert_eq!(focus.participant.original.wait.as_ref().unwrap().native_type.as_deref(), Some("CXCONSUMER"));
    assert_eq!(focus.task.wait_type, "PAGELATCH_EX");
    assert!(question.page_wait().is_none());
    assert!(focus.matches_activity_frame(&scenario.frame(true)));
    assert!(focus.matches_page(&scenario.page_source, &scenario.page, &scenario.page_ref));
}

#[test]
fn holder_session_authority_is_checked_against_every_original_mars_peer() {
    for lifecycle in [SessionLifecycle::Unknown, SessionLifecycle::Connecting, SessionLifecycle::PoolResetPrelogin] {
        let mut scenario = Scenario::new();
        let DiagnosticFocusPlan::Ready(question) = scenario.plan(true).unwrap() else { panic!("original holder") };
        let DiagnosticFocus::PageTaskHolders { mut focus } = *question else { panic!("holder question") };
        let mut peer = scenario.rows[0].clone();
        peer.key.request.as_mut().unwrap().id = "1".into();
        peer.session_context = Some(Box::new(ActivitySessionContext {
            lifecycle,
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: None,
        }));
        scenario.rows.push(peer);
        let SourceObservation::Activity(native) = &mut scenario.source.observation else { panic!("Activity") };
        native.native_rows = 2;
        native.observations = 2;
        native.coverage = StateSourceCoverage::collected(scenario.source.source.clone(), 2);
        scenario.rebind_frame();
        focus.participant.parent = scenario.refs[0].clone();
        let frame = scenario.frame(true);
        assert!(focus.participant.matches_parent(&scenario.source, &scenario.rows[0], &scenario.refs[0]));
        assert_eq!(frame.rows().count(), 2, "canonical raw peer remains retained");
        assert_eq!(focus.matches_activity_frame(&frame), lifecycle == SessionLifecycle::Unknown);
    }
}

#[test]
fn present_page_payload_digest_or_receipt_scope_mismatch_is_integrity_failure_even_when_key_stays_the_same() {
    for field in ["counter", "payload", "scope", "ordinal"] {
        let mut scenario = Scenario::new();
        match field {
            "counter" => scenario.page.task_wait.as_mut().unwrap().wait_duration_ms = Some("1001".into()),
            "payload" => scenario.page_ref.payload_sha256 = digest(b"not-the-original-page"),
            "scope" => scenario.page_ref.source_scope_id = digest(b"another-scope"),
            _ => scenario.page_ref.row_ordinal = 1,
        }
        assert!(scenario.plan(true).is_err(), "{field}");
    }
}

#[test]
fn qualified_later_page_cannot_upgrade_unknown_original_member_role_or_partial_activity_read() {
    let scenario = Scenario::new();
    assert_eq!(
        scenario.plan(false).unwrap(),
        DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::EvidenceUnavailable)
    );
    for field in ["owner", "role"] {
        let mut scenario = Scenario::new();
        match field {
            "owner" => scenario.source.partition = SourcePartition::Target,
            _ => {
                if let SourceObservation::Activity(frame) = &mut scenario.source.observation {
                    frame.database_writable = None;
                }
            }
        }
        scenario.rebind_frame();
        assert!(matches!(scenario.plan(true).unwrap(), DiagnosticFocusPlan::Unavailable(_)), "{field}");
    }
}

#[test]
fn different_original_generation_statement_native_member_epoch_or_postdated_frame_never_gets_task_authority() {
    for field in ["generation", "statement", "member", "epoch", "clock"] {
        let mut scenario = Scenario::new();
        match field {
            "generation" => scenario.rows[0].key.request.as_mut().unwrap().generation = "later-request".into(),
            "statement" => scenario.rows[0].statement_identity = Some(NativeIdentity::new("different-native-token")),
            "member" => {
                if let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut scenario.source.partition {
                    member.native.id = "other-member".into();
                }
            }
            "epoch" => {
                if let SourcePartition::InstanceLocal { epoch, .. } = &mut scenario.source.partition {
                    *epoch = Some("another-native-epoch".into());
                }
                if let SourceObservation::Activity(frame) = &mut scenario.source.observation {
                    frame.epoch = Some("another-native-epoch".into());
                }
            }
            _ => {
                if let SourceObservation::Activity(frame) = &mut scenario.source.observation {
                    frame.observed_from = "2026-11-13T10:02:00Z".into();
                    frame.at = frame.observed_from.clone();
                    frame.observed_until = "2026-11-13T10:02:00.1Z".into();
                }
            }
        }
        scenario.rebind_frame();
        assert!(!matches!(scenario.plan(true), Ok(DiagnosticFocusPlan::Ready(_))), "{field}");
    }
}

#[test]
fn io_tasks_and_absent_statement_binding_remain_distinct_from_an_admitted_holder_question() {
    let mut io = Scenario::new();
    io.page.resource.kind = WaitResourceKind::PageIo;
    let task = io.page.task_wait.as_mut().unwrap();
    task.wait_type = "PAGEIOLATCH_SH".into();
    task.resource = io.page.resource.clone();
    io.rebind_page();
    assert_eq!(io.plan(true).unwrap(), DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::Unsupported));
    let mut missing = Scenario::new();
    missing.page.statement_identity = None;
    missing.page.plan_identity = None;
    missing.rebind_page();
    assert_eq!(
        missing.plan(true).unwrap(),
        DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity)
    );
}

#[test]
fn original_page_role_is_native_context_and_cannot_be_filled_by_an_activity_or_holder_role() {
    let scenario = Scenario::new();
    assert_eq!(NativeReadContext::from_snapshot(&scenario.page_source).unwrap().database_writable, Some(true));
    for role in [None, Some(false)] {
        let mut changed = Scenario::new();
        let SourceObservation::PageWaitTargets { database_writable, .. } = &mut changed.page_source.observation else {
            panic!("typed native role")
        };
        *database_writable = role;
        changed.rebind_page();
        if role.is_none() {
            assert_eq!(
                changed.plan(true).unwrap(),
                DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::EvidenceUnavailable)
            );
        } else {
            assert_eq!(
                changed.plan(true).unwrap(),
                DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::SourceChanged),
                "well-formed native role changed after the original capture"
            );
        }
    }
    let mut generic = scenario.page_source.clone();
    generic.observation = SourceObservation::Snapshot(generic.observation.snapshot().unwrap().clone());
    assert!(!generic.observation.covers_request(), "a current page source requires its typed context");
    let row = TelemetryRecord::Operational(OperationalObservation {
        source: scenario.page.source.clone(),
        key: scenario.page.key().unwrap(),
        fact: OperationalFact::PageWait(Box::new(scenario.page.clone())),
    });
    assert!(ExportedRecord::new(&generic, &scenario.page_ref.capture, row).is_err());
}

#[test]
fn a_shared_page_header_cannot_borrow_the_application_databases_role_or_claim_a_complete_task_population() {
    let scenario = Scenario::new();
    let mut shared = scenario.page_source.clone();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &shared.partition else {
        panic!("native member")
    };
    shared.target = CollectionTarget::Shared(member.clone());
    assert!(!shared.partition_is_consistent(), "application updateability is not shared-tempdb authority");
    let SourceObservation::PageWaitTargets { database_writable, original_activity, .. } = &mut shared.observation
    else {
        panic!()
    };
    *database_writable = None;
    *original_activity = None;
    assert!(shared.partition_is_consistent());
    assert_eq!(NativeReadContext::from_snapshot(&shared).unwrap().database_writable, None);
    let SourceObservation::PageWaitTargets { snapshot, .. } = &mut shared.observation else { panic!() };
    snapshot.sources[0] = StateSourceCoverage::collected(snapshot.sources[0].source.clone(), 1);
    assert!(!shared.observation.covers_request(), "the bounded non-atomic task sample is never a full population");
}

#[test]
fn a_paired_page_cannot_replace_its_original_with_an_identical_native_actor_from_a_different_capture() {
    let mut scenario = Scenario::new();
    let DiagnosticFocusPlan::Ready(original_question) = scenario.plan(true).unwrap() else { panic!() };
    let DiagnosticFocus::PageTaskHolders { focus: original_focus } = *original_question else { panic!() };
    let capture = CaptureIdentity {
        observation_id: "different-otherwise-identical-original-frame".into(),
        revision: Some(CaptureRevision {
            outbox_id: "another-authenticated-original".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    for (ordinal, (row, reference)) in scenario.rows.iter().zip(&mut scenario.refs).enumerate() {
        let exported = ExportedRecord::new(&scenario.source, &capture, TelemetryRecord::from(row.clone())).unwrap();
        *reference = DiagnosticEvidenceRef::from_export(
            &scenario.source,
            &digest(b"different authentic original manifest"),
            &exported,
            ordinal as u32,
        )
        .unwrap();
    }
    assert_ne!(scenario.refs[0], original_focus.participant.parent);
    assert!(
        scenario.plan(true).is_err(),
        "an authenticated Some association cannot be replaced by a later same-native actor"
    );
    let mut substituted = *original_focus;
    substituted.participant.parent = scenario.refs[0].clone();
    assert!(substituted.is_valid());
    assert!(substituted.matches_activity_frame(&scenario.frame(true)));
    assert!(!substituted.matches_page(&scenario.page_source, &scenario.page, &scenario.page_ref));
}
