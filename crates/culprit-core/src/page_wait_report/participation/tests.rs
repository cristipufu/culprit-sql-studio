use super::*;
use crate::telemetry::activity_frame::tests::{borrowed, references, request, source};
use crate::{ObservedWaitResource, WaitResourceKind};

struct Fixture {
    source: CollectionDescription,
    rows: Vec<ActivityObservation>,
    references: Vec<DiagnosticEvidenceRef>,
    original: ActivityRequestFocus,
    page: PageWaitObservation,
}

impl Fixture {
    fn new(owner_contexts: usize) -> Self {
        let mut waiter = request("42");
        let resource = ObservedWaitResource {
            kind: WaitResourceKind::MemoryPage,
            native: NativeIdentity::new("7:1:4144").qualified("source", "native-request-wait-resource"),
        };
        waiter.statement_identity = Some(NativeIdentity::new("original-native-statement"));
        waiter.wait = Some(ActivityWait {
            class: None,
            native_type: Some("PAGELATCH_EX".into()),
            elapsed_ms: Some("1".into()),
            owners: vec![ActivityWaitOwner::Session { id: "57".into(), generation: Some("login-1".into()) }],
            identified_resources: vec![resource.clone()],
            resources: None,
        });
        let mut rows = vec![waiter.clone()];
        for request_index in 0..owner_contexts {
            let mut owner = request("57");
            owner.key.request.as_mut().unwrap().id = request_index.to_string();
            owner.statement_identity = Some(NativeIdentity::new(format!("owner-statement-{request_index}")));
            owner.command = Some(if request_index == 0 { "INSERT" } else { "SELECT" }.into());
            owner.statement = DefinitionPayload::sql("private owner SQL 9264", crate::Engine::Mssql);
            rows.push(owner);
        }
        let mut source = source(rows.len());
        source.partition = SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: crate::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("original-machine"),
                native: NativeIdentity::new("original-member"),
            }),
            epoch: Some("original-native-epoch".into()),
        };
        let references = references(&source, &rows);
        let SourceObservation::Activity(native) = &source.observation else { unreachable!() };
        let original = ActivityRequestFocus {
            parent: references[0].clone(),
            target: source.target.clone(),
            partition: source.partition.clone(),
            database_writable: native.database_writable,
            source: source.source.clone(),
            epoch: native.epoch.clone().unwrap(),
            original_observed_at: native.at.clone(),
            original: waiter.clone(),
            statement_identity: waiter.statement_identity.clone().unwrap(),
        };
        let page = PageWaitObservation {
            source: "page-wait-targets".into(),
            observed_from: "2026-11-13T10:00:01Z".into(),
            observed_until: "2026-11-13T10:00:02Z".into(),
            request: waiter,
            task_wait: None,
            before_check: None,
            after_check: None,
            statement_identity: None,
            plan_identity: None,
            resource,
            observed_address: None,
            resolution: PageWaitResolution::Unavailable,
            target: None,
            index_operations: None,
        };
        assert!(original.is_valid());
        assert!(page.is_valid());
        Self { source, rows, references, original, page }
    }

    fn explain(&self) -> Result<PageWaitBlockingParticipation> {
        self.explain_rows(&self.source, &self.rows, &self.references, true)
    }

    fn page_source(&self) -> CollectionDescription {
        CollectionDescription {
            target: self.original.target.clone(),
            partition: self.original.partition.clone(),
            source: self.page.source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PageWaitTargets {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageWaitTargets,
                    observed_from: self.page.observed_from.clone(),
                    observed_until: self.page.observed_until.clone(),
                    records: 1,
                    sources: vec![StateSourceCoverage::observed(self.page.source.clone(), 1, false)],
                },
                database_writable: None,
                original_activity: None,
            },
        }
    }

    fn explain_rows(
        &self,
        source: &CollectionDescription,
        rows: &[ActivityObservation],
        references: &[DiagnosticEvidenceRef],
        complete: bool,
    ) -> Result<PageWaitBlockingParticipation> {
        let frame = ActivityFrameEvidence::new(source, &borrowed(rows, references), complete)?;
        page_wait_blocking_participation(&self.page_source(), &self.page, &self.original, &frame)
    }
}

#[test]
fn engine_reported_session_participation_keeps_every_mars_context_and_exact_original_receipt() {
    let fixture = Fixture::new(2);
    let result = fixture.explain().unwrap();
    assert_eq!(result.context_count, 2);
    assert_eq!(result.contexts.len(), 2);
    assert_eq!(result.references().cloned().collect::<Vec<_>>(), fixture.references[1..]);
    assert_eq!(result.contexts[0].command.as_deref(), Some("INSERT"));
    assert_eq!(result.contexts[1].command.as_deref(), Some("SELECT"));
    assert!(result.issues.contains(&PageWaitBlockingIssue::MultipleOwnerContexts));
    let basis = result.basis.as_ref().unwrap();
    assert_eq!(basis.waiter.evidence.as_ref(), Some(&fixture.original.parent));
    assert_eq!(basis.observed_at, fixture.original.original_observed_at);
    assert_ne!(basis.observed_at, fixture.page.observed_from);
    let encoded = serde_json::to_string(&result).unwrap();
    assert!(!encoded.contains("private owner SQL"));
    assert!(!encoded.contains("\"request_counters\""));
    assert_eq!(serde_json::from_str::<PageWaitBlockingParticipation>(&encoded).unwrap(), result);
}

#[test]
fn target_namespace_and_known_native_epoch_do_not_establish_a_physical_member() {
    let mut fixture = Fixture::new(1);
    fixture.source.partition = SourcePartition::Target;
    fixture.references = references(&fixture.source, &fixture.rows);
    fixture.original.partition = SourcePartition::Target;
    fixture.original.parent = fixture.references[0].clone();
    let result = fixture.explain().unwrap();
    assert_eq!(result.reported_owners, fixture.original.original.wait.as_ref().unwrap().owners);
    assert!(result.contexts.is_empty());
    assert_eq!(result.references().count(), 0);
    assert!(result.issues.contains(&PageWaitBlockingIssue::NativeScopeUnqualified));
}

#[test]
fn later_frame_same_ids_or_changed_member_role_epoch_cannot_replace_original_parent() {
    let fixture = Fixture::new(1);
    for field in ["clock", "member", "epoch", "role", "target", "source"] {
        let mut changed = fixture.source.clone();
        let SourceObservation::Activity(native) = &mut changed.observation else { unreachable!() };
        match field {
            "clock" => {
                native.observed_from = "2026-11-13T10:00:00.5Z".into();
                native.at = "2026-11-13T10:00:00.55Z".into();
                native.observed_until = "2026-11-13T10:00:00.6Z".into();
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition else {
                    unreachable!()
                };
                member.native.id = "later-native-member".into();
            }
            "epoch" => {
                native.epoch = Some("later-native-epoch".into());
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { unreachable!() };
                *epoch = native.epoch.clone();
            }
            "role" => native.database_writable = Some(false),
            "target" => {
                changed.target =
                    crate::DatabaseIdentity::server(crate::Engine::Mssql, "sample", "another.invalid", 1433, None)
                        .into()
            }
            _ => {
                changed.source = "later-source".into();
                native.coverage.source = changed.source.clone();
            }
        }
        assert!(changed.observation.covers_request(), "coherent {field} header");
        assert!(changed.partition_is_consistent(), "coherent {field} partition");
        let changed_refs = references(&changed, &fixture.rows);
        assert!(fixture.explain_rows(&changed, &fixture.rows, &changed_refs, true).is_err(), "{field}");
    }
    let mut later_rows = fixture.rows.clone();
    later_rows[0].state = "running".into();
    later_rows[0].request_counters.as_mut().unwrap().cpu_ms = Some("999".into());
    assert!(
        fixture.explain_rows(&fixture.source, &later_rows, &references(&fixture.source, &later_rows), true).is_err()
    );
}

#[test]
fn matching_numeric_actors_on_another_page_source_member_epoch_database_or_window_are_rejected() {
    let fixture = Fixture::new(1);
    let frame =
        ActivityFrameEvidence::new(&fixture.source, &borrowed(&fixture.rows, &fixture.references), true).unwrap();
    for field in ["member", "epoch", "database", "source", "bounds", "dataset"] {
        let mut source = fixture.page_source();
        match field {
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    unreachable!()
                };
                member.native.id = "another-native-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { unreachable!() };
                *epoch = Some("another-native-process".into());
            }
            "database" => {
                source.target =
                    crate::DatabaseIdentity::server(crate::Engine::Mssql, "other", "fixture.invalid", 1433, None).into()
            }
            _ => {
                let SourceObservation::PageWaitTargets { snapshot: native, .. } = &mut source.observation else {
                    unreachable!()
                };
                if field == "bounds" {
                    native.observed_from = fixture.page.observed_until.clone();
                } else if field == "source" {
                    native.sources[0].source = "another-native-page-source".into();
                } else {
                    native.dataset = SnapshotDataset::Operational;
                }
            }
        }
        assert!(
            page_wait_blocking_participation(&source, &fixture.page, &fixture.original, &frame).is_err(),
            "{field}"
        );
    }
}

#[test]
fn recipe_namespace_may_differ_from_the_exact_declared_native_page_source() {
    let fixture = Fixture::new(1);
    let frame =
        ActivityFrameEvidence::new(&fixture.source, &borrowed(&fixture.rows, &fixture.references), true).unwrap();
    let mut source = fixture.page_source();
    source.source = "separate-recipe-source".into();
    let result = page_wait_blocking_participation(&source, &fixture.page, &fixture.original, &frame).unwrap();
    assert_eq!(result.contexts.len(), 1);
    let mut undeclared = fixture.page.clone();
    undeclared.source = "undeclared-native-source".into();
    assert!(page_wait_blocking_participation(&source, &undeclared, &fixture.original, &frame).is_err());
}

#[test]
fn focused_result_must_name_the_same_original_receipt_and_cannot_precede_it() {
    let mut fixture = Fixture::new(1);
    let frame =
        ActivityFrameEvidence::new(&fixture.source, &borrowed(&fixture.rows, &fixture.references), true).unwrap();
    let mut source = fixture.page_source();
    let SourceObservation::PageWaitTargets { snapshot, .. } = source.observation else { unreachable!() };
    source.observation = SourceObservation::FocusedPageWait {
        snapshot,
        result: Box::new(ActivityWaitFocusResult {
            focus: ActivityWaitFocus { participant: fixture.original.clone(), resource: fixture.page.resource.clone() },
            observed_from: fixture.page.observed_from.clone(),
            observed_until: fixture.page.observed_until.clone(),
            source: FocusSourceState::Matched,
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        }),
    };
    assert_eq!(
        page_wait_blocking_participation(&source, &fixture.page, &fixture.original, &frame).unwrap().contexts.len(),
        1
    );
    let SourceObservation::FocusedPageWait { result, .. } = &mut source.observation else { unreachable!() };
    result.focus.participant_mut().parent.manifest_sha256 = digest(b"another original receipt");
    assert!(page_wait_blocking_participation(&source, &fixture.page, &fixture.original, &frame).is_err());
    fixture.page.observed_from = "2026-11-13T09:59:59Z".into();
    fixture.page.observed_until = "2026-11-13T10:00:00Z".into();
    assert!(fixture.page.is_valid());
    assert!(
        page_wait_blocking_participation(&fixture.page_source(), &fixture.page, &fixture.original, &frame).is_err()
    );
}

#[test]
fn missing_generation_unknown_owner_or_recycled_owner_id_never_attach_current_sql() {
    for owner in [
        ActivityWaitOwner::Session { id: "57".into(), generation: None },
        ActivityWaitOwner::Session { id: "57".into(), generation: Some("previous-login".into()) },
        ActivityWaitOwner::Unknown { native_id: "-5".into(), reason: UnknownWaitOwner::UntrackedLatch },
        ActivityWaitOwner::Unknown { native_id: "-4".into(), reason: UnknownWaitOwner::LatchTransition },
    ] {
        let mut fixture = Fixture::new(1);
        fixture.rows[0].wait.as_mut().unwrap().owners = vec![owner.clone()];
        fixture.references = references(&fixture.source, &fixture.rows);
        fixture.original.original = fixture.rows[0].clone();
        fixture.original.parent = fixture.references[0].clone();
        let result = fixture.explain().unwrap();
        assert_eq!(result.reported_owners, [owner]);
        assert!(result.contexts.is_empty());
        assert_eq!(result.references().count(), 0);
        assert!(!result.issues.is_empty());
    }
}

#[test]
fn parallel_task_pointer_and_coordinator_owner_cannot_establish_task_holder_context() {
    let mut fixture = Fixture::new(1);
    let resource = ObservedWaitResource {
        kind: WaitResourceKind::MemoryPage,
        native: NativeIdentity::new("2:1:4144").qualified("source", "sys.dm_os_waiting_tasks.resource_description"),
    };
    fixture.page.resource = resource.clone();
    fixture.page.task_wait = Some(MssqlPageTaskWait {
        observed_at: fixture.page.observed_from.clone(),
        session_id: 42,
        request_id: 0,
        execution_context_id: 3,
        task_address: "0000000000000001".into(),
        wait_type: "PAGELATCH_EX".into(),
        wait_duration_ms: Some("1".into()),
        resource,
        resource_address: Some("0000000000000002".into()),
        blocking_task_address: Some("0000000000000003".into()),
        blocking_session_id: Some(57),
        blocking_execution_context_id: Some(0),
    });
    fixture.page.request.wait.as_mut().unwrap().native_type = Some("CXCONSUMER".into());
    assert!(fixture.page.is_valid());
    let result = fixture.explain().unwrap();
    assert!(result.contexts.is_empty());
    assert!(result.reported_owners.is_empty());
    assert_eq!(result.context_count, 0);
    assert!(result.issues.contains(&PageWaitBlockingIssue::ParallelTaskUnresolved));
}

#[test]
fn oversized_all_mars_set_is_withheld_instead_of_selecting_the_first_request() {
    let fixture = Fixture::new(MAX_PAGE_WAIT_BLOCKING_CONTEXTS + 1);
    let result = fixture.explain().unwrap();
    assert_eq!(result.context_count, MAX_PAGE_WAIT_BLOCKING_CONTEXTS as u32 + 1);
    assert!(result.contexts.is_empty());
    assert_eq!(result.references().count(), 0);
    assert!(result.issues.contains(&PageWaitBlockingIssue::ContextsLimited));
    let complete = Fixture::new(MAX_PAGE_WAIT_BLOCKING_CONTEXTS).explain().unwrap();
    assert_eq!(complete.contexts.len(), MAX_PAGE_WAIT_BLOCKING_CONTEXTS);
}

#[test]
fn incomplete_archive_or_native_population_preserves_positive_historical_context_without_absence() {
    let fixture = Fixture::new(2);
    let result = fixture.explain_rows(&fixture.source, &fixture.rows[..2], &fixture.references[..2], false).unwrap();
    assert_eq!(result.contexts.len(), 1);
    assert!(result.issues.contains(&PageWaitBlockingIssue::ReadIncomplete));
    let mut partial = fixture.source.clone();
    let SourceObservation::Activity(native) = &mut partial.observation else { unreachable!() };
    native.coverage = StateSourceCoverage::observed(partial.source.clone(), fixture.rows.len() as u32, false);
    let refs = references(&partial, &fixture.rows);
    let mut original = fixture.original.clone();
    original.parent = refs[0].clone();
    let frame = ActivityFrameEvidence::new(&partial, &borrowed(&fixture.rows, &refs), true).unwrap();
    let result = page_wait_blocking_participation(&fixture.page_source(), &fixture.page, &original, &frame).unwrap();
    assert_eq!(result.contexts.len(), 2);
    assert!(result.issues.contains(&PageWaitBlockingIssue::NativePopulationPartial));
}

#[test]
fn partial_original_frame_missing_waiter_keeps_page_evidence_without_borrowing_focus_copies() {
    let fixture = Fixture::new(2);
    for retained in [0..0, 1..fixture.rows.len()] {
        let result = fixture
            .explain_rows(
                &fixture.source,
                &fixture.rows[retained.clone()],
                &fixture.references[retained.clone()],
                false,
            )
            .unwrap();
        assert!(result.basis.is_none());
        assert!(result.reported_owners.is_empty());
        assert!(result.contexts.is_empty());
        assert_eq!(result.references().count(), 0);
        assert_eq!(result.issues, [PageWaitBlockingIssue::FrameUnavailable, PageWaitBlockingIssue::ReadIncomplete]);
        assert!(
            fixture
                .explain_rows(&fixture.source, &fixture.rows[retained.clone()], &fixture.references[retained], true)
                .is_err()
        );
    }
    let mut wrong_source = fixture.source.clone();
    let SourceObservation::Activity(native) = &mut wrong_source.observation else { unreachable!() };
    native.database_writable = Some(false);
    let wrong_refs = references(&wrong_source, &fixture.rows);
    assert!(fixture.explain_rows(&wrong_source, &fixture.rows[1..], &wrong_refs[1..], false).is_err());
    let mut wrong_refs = fixture.references.clone();
    for reference in &mut wrong_refs {
        reference.manifest_sha256 = digest(b"another capture manifest");
    }
    assert!(fixture.explain_rows(&fixture.source, &fixture.rows[1..], &wrong_refs[1..], false).is_err());
}

#[test]
fn absent_original_cannot_have_a_changed_copy_or_an_ordinal_outside_its_authenticated_header() {
    let fixture = Fixture::new(2);
    let frame =
        ActivityFrameEvidence::new(&fixture.source, &borrowed(&fixture.rows[1..], &fixture.references[1..]), false)
            .unwrap();
    let source = fixture.page_source();
    assert!(
        PageWaitActivityEvidence::Focused { source: &source, original: &fixture.original, frame: &frame }
            .validate()
            .is_ok()
    );
    for field in ["copy", "ordinal"] {
        let mut changed = fixture.original.clone();
        if field == "copy" {
            changed.original.command = Some("DELETE".into());
        } else {
            changed.parent.row_ordinal = frame.native().observations;
        }
        assert!(changed.is_valid());
        assert!(
            PageWaitActivityEvidence::Focused { source: &source, original: &changed, frame: &frame }
                .validate()
                .is_err(),
            "{field}"
        );
        assert!(page_wait_blocking_participation(&source, &fixture.page, &changed, &frame).is_err(), "{field}");
    }
}

#[test]
fn idle_owner_has_session_context_without_previous_statement_or_invented_request() {
    let mut fixture = Fixture::new(1);
    let owner = &mut fixture.rows[1];
    owner.key.request = None;
    owner.request_counters = None;
    owner.statement_identity = None;
    owner.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    owner.idle = true;
    owner.command = None;
    owner.state = "sleeping".into();
    fixture.references = references(&fixture.source, &fixture.rows);
    fixture.original.parent = fixture.references[0].clone();
    let result = fixture.explain().unwrap();
    assert!(result.contexts[0].reference.activity.request.is_none());
    assert!(result.contexts[0].reference.statement.is_none());
    assert!(result.contexts[0].command.is_none());
    assert_eq!(result.contexts[0].state, "sleeping");
}

fn paired_task(fixture: &Fixture) -> (CollectionDescription, PageWaitObservation) {
    let mut page = fixture.page.clone();
    page.resource.native =
        page.resource.native.clone().qualified("source", "sys.dm_os_waiting_tasks.resource_description");
    page.task_wait = Some(MssqlPageTaskWait {
        observed_at: page.observed_from.clone(),
        session_id: page.request.key.session.id.parse().unwrap(),
        request_id: page.request.key.request.as_ref().unwrap().id.parse().unwrap(),
        execution_context_id: 1,
        task_address: "00000000000000AB".into(),
        wait_type: "PAGELATCH_EX".into(),
        wait_duration_ms: Some("0".into()),
        resource: page.resource.clone(),
        resource_address: None,
        blocking_task_address: None,
        blocking_session_id: None,
        blocking_execution_context_id: None,
    });
    let mut source = fixture.page_source();
    let SourceObservation::PageWaitTargets { database_writable, original_activity, .. } = &mut source.observation
    else {
        unreachable!()
    };
    *database_writable = Some(true);
    *original_activity = Some(fixture.references[0].capture_ref());
    assert!(page.fits_source_scope(&source));
    (source, page)
}

#[test]
fn task_recorded_context_uses_the_initial_token_and_full_native_request_generation() {
    let fixture = Fixture::new(0);
    let frame =
        ActivityFrameEvidence::new(&fixture.source, &borrowed(&fixture.rows, &fixture.references), true).unwrap();
    let (source, page) = paired_task(&fixture);
    let evidence = PageWaitActivityEvidence::Tasks { source: &source, frame: &frame };
    assert!(page.statement_identity.is_none());
    let original = evidence.recorded_task_request(&page).unwrap().unwrap();
    assert_eq!(original.observation, &fixture.rows[0]);
    assert_eq!(original.reference, Some(&fixture.references[0]));
    for field in ["missing-token", "different-token", "login", "request-generation", "negative-initial-session"] {
        let mut changed = page.clone();
        match field {
            "missing-token" => changed.request.statement_identity = None,
            "different-token" => changed.request.statement_identity = Some(NativeIdentity::new("foreign-statement")),
            "login" => changed.request.key.session.generation.push_str("-reused"),
            "request-generation" => changed.request.key.request.as_mut().unwrap().generation.push_str("-reused"),
            "negative-initial-session" => {
                changed.request.session_context = Some(Box::new(ActivitySessionContext {
                    lifecycle: SessionLifecycle::PoolResetPrelogin,
                    login_age_ms: None,
                    last_request_end_age_ms: None,
                    client_library: None,
                    client_process_id: None,
                    native: None,
                }))
            }
            _ => unreachable!(),
        }
        assert!(evidence.recorded_task_request(&changed).unwrap().is_none(), "{field}");
    }
}

#[test]
fn task_original_frame_requires_the_declared_capture_scope_owner_role_and_ordered_native_clocks() {
    let fixture = Fixture::new(0);
    let frame =
        ActivityFrameEvidence::new(&fixture.source, &borrowed(&fixture.rows, &fixture.references), true).unwrap();
    let (source, _) = paired_task(&fixture);
    for field in
        ["capture", "manifest", "scope", "member", "epoch", "database", "role", "unknown-role", "clock", "unpaired"]
    {
        let mut changed = source.clone();
        let SourceObservation::PageWaitTargets { snapshot, original_activity, database_writable } =
            &mut changed.observation
        else {
            unreachable!()
        };
        match field {
            "capture" => original_activity.as_mut().unwrap().capture.observation_id.push_str("-foreign"),
            "manifest" => original_activity.as_mut().unwrap().manifest_sha256 = digest(b"foreign manifest"),
            "scope" => original_activity.as_mut().unwrap().source_scope_id = digest(b"foreign scope"),
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition else {
                    unreachable!()
                };
                member.native.id.push_str("-foreign");
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { unreachable!() };
                *epoch = Some("foreign-epoch".into());
            }
            "database" => {
                let CollectionTarget::Database(database) = &mut changed.target else { unreachable!() };
                database.database.push_str("_foreign");
            }
            "role" => *database_writable = Some(false),
            "unknown-role" => *database_writable = None,
            "clock" => snapshot.observed_from = "2026-11-13T09:59:59Z".into(),
            "unpaired" => *original_activity = None,
            _ => unreachable!(),
        }
        assert!(PageWaitActivityEvidence::Tasks { source: &changed, frame: &frame }.validate().is_err(), "{field}");
    }
}

#[test]
fn partial_original_bodies_and_negative_mars_peers_never_supply_recorded_task_sql() {
    let mut fixture = Fixture::new(1);
    let (source, page) = paired_task(&fixture);
    for present in [0, 1] {
        let rows = borrowed(&fixture.rows[..present], &fixture.references[..present]);
        let frame = ActivityFrameEvidence::new(&fixture.source, &rows, false).unwrap();
        let evidence = PageWaitActivityEvidence::Tasks { source: &source, frame: &frame };
        assert!(evidence.recorded_task_request(&page).unwrap().is_none());
    }
    let mut peer = fixture.rows[0].clone();
    peer.key.request.as_mut().unwrap().id = "1".into();
    peer.session_context = Some(Box::new(ActivitySessionContext {
        lifecycle: SessionLifecycle::PoolResetPrelogin,
        login_age_ms: None,
        last_request_end_age_ms: None,
        client_library: None,
        client_process_id: None,
        native: None,
    }));
    fixture.rows[1] = peer;
    fixture.references = references(&fixture.source, &fixture.rows);
    let frame =
        ActivityFrameEvidence::new(&fixture.source, &borrowed(&fixture.rows, &fixture.references), true).unwrap();
    assert!(!frame.has_session_generation_authority(&page.request.key.session));
    assert!(
        PageWaitActivityEvidence::Tasks { source: &source, frame: &frame }
            .recorded_task_request(&page)
            .unwrap()
            .is_none()
    );
}
