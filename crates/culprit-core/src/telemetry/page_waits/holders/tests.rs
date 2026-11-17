use super::*;
mod planning;
mod resources;
mod source;

pub(crate) fn fixture() -> (PageTaskHolderFocus, NativeReadContext, PageTaskHolderObservation) {
    let (writer_focus, mut context, writer) = super::super::tests::writer_fixture();
    let mut original_page = *writer.page.unwrap();
    let resource = ObservedWaitResource {
        kind: WaitResourceKind::MemoryPage,
        native: NativeIdentity::new("5:1:42").qualified("source", "sys.dm_os_waiting_tasks.resource_description"),
    };
    let mut task = MssqlPageTaskWait {
        observed_at: original_page.observed_from.clone(),
        session_id: 42,
        request_id: original_page.request.key.request.as_ref().unwrap().id.parse().unwrap(),
        execution_context_id: 2,
        task_address: "0000000000000042".into(),
        wait_type: "PAGELATCH_EX".into(),
        wait_duration_ms: Some("1000".into()),
        resource,
        resource_address: Some("0000000000000044".into()),
        blocking_task_address: Some("0000000000000043".into()),
        blocking_session_id: Some(43),
        blocking_execution_context_id: Some(0),
    };
    original_page.resource = task.resource.clone();
    original_page.task_wait = Some(task.clone());
    let mut reference = writer_focus.page;
    reference.native_key =
        NativeRecordIdentity::Operational { source: original_page.source.clone(), key: original_page.key().unwrap() };
    let focus = PageTaskHolderFocus {
        participant: writer_focus.waiter.participant,
        page: reference,
        address: original_page.observed_address.unwrap(),
        task: task.clone(),
        target: original_page.target.clone().unwrap(),
        original_observed_until: original_page.observed_until.clone(),
    };
    context.observed_from = "2026-11-13T10:01:00Z".into();
    context.observed_until = "2026-11-13T10:01:01Z".into();
    let mut page = original_page;
    page.observed_from = "2026-11-13T10:01:00.2Z".into();
    page.observed_until = "2026-11-13T10:01:00.8Z".into();
    page.before_check.as_mut().unwrap().observed_at = "2026-11-13T10:01:00.3Z".into();
    page.after_check.as_mut().unwrap().observed_at = "2026-11-13T10:01:00.7Z".into();
    task.observed_at = page.observed_from.clone();
    page.task_wait = Some(task.clone());
    let owner = writer.candidates[0].request.clone().unwrap();
    let holder = MssqlPageTaskHolder {
        relation: PageTaskHolderRelation::QualifiedRequest,
        matching_owner_rows: 1,
        task: Some(MssqlPageTaskIdentity {
            task_address: "0000000000000043".into(),
            session_id: Some(43),
            request_id: Some(owner.key.request.as_ref().unwrap().id.parse().unwrap()),
            execution_context_id: 0,
        }),
        request: Some(Box::new(owner)),
    };
    let before = MssqlPageTaskHolderPoint {
        observed_at: task.observed_at.clone(),
        database_writable: Some(true),
        request: PageWaitRequestState::SameStatement,
        wait: PageWaitRelation::OriginalObserved,
        holder: holder.clone(),
    };
    task.observed_at = "2026-11-13T10:01:00.9Z".into();
    let after = MssqlPageTaskHolderPoint { observed_at: task.observed_at.clone(), ..before.clone() };
    let observation = PageTaskHolderObservation {
        observed_from: context.observed_from.clone(),
        observed_until: context.observed_until.clone(),
        focus_key: focus.key().unwrap(),
        target_state: PageWriterTargetState::CurrentMapped,
        final_target: Some(Box::new(PageTaskHolderTargetCheck {
            observed_at: "2026-11-13T10:01:00.85Z".into(),
            target: page.target.clone().unwrap(),
        })),
        page: Some(Box::new(page)),
        before,
        after_task: Some(task),
        after,
    };
    assert!(focus.accepts_observation(&context, &observation));
    (focus, context, observation)
}

#[test]
fn task_holder_keeps_native_task_and_exact_request_generations_without_replacing_the_waiter() {
    let (focus, context, value) = fixture();
    let decoded: PageTaskHolderObservation = serde_json::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(focus.accepts_observation(&context, &decoded));
    assert_ne!(decoded.before.holder.request.as_ref().unwrap().key, focus.participant.original.key);
    assert_eq!(decoded.page.as_ref().unwrap().task_wait.as_ref().unwrap().task_address, focus.task.task_address);
    assert_ne!(decoded.before.observed_at, decoded.after.observed_at);
    assert_eq!(decoded.after_task.as_ref().unwrap().observed_at, decoded.after.observed_at);
}

#[test]
fn point_holders_can_change_without_rewriting_the_first_native_holder() {
    let (focus, context, mut value) = fixture();
    let holder = &mut value.after.holder;
    holder.task.as_mut().unwrap().task_address = "0000000000000045".into();
    holder.task.as_mut().unwrap().session_id = Some(45);
    holder.request.as_mut().unwrap().key.session.id = "45".into();
    value.after_task.as_mut().unwrap().blocking_task_address = Some("0000000000000045".into());
    value.after_task.as_mut().unwrap().blocking_session_id = Some(45);
    assert!(focus.accepts_observation(&context, &value));
    assert_eq!(value.before.holder.request.as_ref().unwrap().key.session.id, "43");
    assert_eq!(value.after.holder.request.as_ref().unwrap().key.session.id, "45");
}

#[test]
fn pointer_only_pid_only_ambiguous_and_sentinel_evidence_never_acquire_a_request() {
    let (focus, context, value) = fixture();
    for relation in [
        PageTaskHolderRelation::TaskOnly,
        PageTaskHolderRelation::Sentinel,
        PageTaskHolderRelation::Ambiguous,
        PageTaskHolderRelation::NotReported,
        PageTaskHolderRelation::SelfReference,
        PageTaskHolderRelation::Changed,
    ] {
        let mut changed = value.clone();
        let holder = &mut changed.after.holder;
        holder.relation = relation;
        holder.request = None;
        match relation {
            PageTaskHolderRelation::Ambiguous => {
                holder.matching_owner_rows = 2;
                holder.task = None;
            }
            PageTaskHolderRelation::Sentinel => {
                holder.matching_owner_rows = 0;
                holder.task = None;
                changed.after_task.as_mut().unwrap().blocking_session_id = Some(-4);
            }
            PageTaskHolderRelation::NotReported => {
                holder.matching_owner_rows = 0;
                holder.task = None;
                changed.after_task.as_mut().unwrap().blocking_task_address = None;
            }
            PageTaskHolderRelation::SelfReference => {
                holder.matching_owner_rows = 0;
                holder.task = None;
                changed.after_task.as_mut().unwrap().blocking_task_address = Some(focus.task.task_address.clone());
            }
            _ => {}
        }
        assert!(focus.accepts_observation(&context, &changed), "{relation:?}");
        changed.after.holder.request = value.after.holder.request.clone();
        assert!(!changed.is_valid(), "{relation:?} must not attach a current request");
    }
}

#[test]
fn task_request_member_role_database_mapping_and_clock_tampering_cannot_borrow_original_authority() {
    let (focus, context, value) = fixture();
    for field in ["owner-session", "owner-request", "owner-task", "waiter-task", "clock", "role", "target"] {
        let mut changed = value.clone();
        match field {
            "owner-session" => changed.after.holder.request.as_mut().unwrap().key.session.id = "99".into(),
            "owner-request" => {
                changed.after.holder.request.as_mut().unwrap().key.request.as_mut().unwrap().id = "99".into()
            }
            "owner-task" => changed.after.holder.task.as_mut().unwrap().task_address = "0000000000000099".into(),
            "waiter-task" => changed.after_task.as_mut().unwrap().task_address = "0000000000000099".into(),
            "clock" => changed.after.observed_at = "2026-11-13T09:00:00Z".into(),
            "role" => changed.after.database_writable = Some(false),
            _ => changed
                .page
                .as_mut()
                .unwrap()
                .target
                .as_mut()
                .unwrap()
                .object
                .as_mut()
                .unwrap()
                .native
                .qualifiers
                .insert("modified_without_offset".into(), "2020-02-01T00:00:00".into())
                .map(|_| ())
                .unwrap_or(()),
        }
        assert!(!focus.accepts_observation(&context, &changed), "{field}");
    }
    for field in ["member", "epoch", "role", "database"] {
        let mut changed = context.clone();
        match field {
            "member" => {
                if let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition {
                    member.native.id = "other".into();
                }
            }
            "epoch" => {
                if let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition {
                    *epoch = Some("other".into());
                }
            }
            "role" => changed.database_writable = None,
            _ => {
                changed.target =
                    crate::DatabaseIdentity::server(crate::Engine::Mssql, "other", "fixture.invalid", 1433, None).into()
            }
        }
        assert!(!focus.accepts_observation(&changed, &value), "{field}");
    }
}

#[test]
fn task_request_identity_is_independent_of_sql_payload_availability_and_coordinator_wait() {
    let (mut focus, context, mut value) = fixture();
    focus.participant.original.wait = None;
    value.focus_key = focus.key().unwrap();
    for point in [&mut value.before, &mut value.after] {
        point.holder.request.as_mut().unwrap().statement =
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    }
    assert!(focus.accepts_observation(&context, &value));
    value.after.request = PageWaitRequestState::Changed;
    value.after.wait = PageWaitRelation::Unqualified;
    assert!(!focus.accepts_observation(&context, &value));
}
