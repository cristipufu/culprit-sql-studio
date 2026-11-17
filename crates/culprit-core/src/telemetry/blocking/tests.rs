use super::super::activity_frame::tests as frames;
use super::*;
use crate::{Engine, MeasurementScope};

mod maintenance;

pub(crate) fn fixture(
    qualified_parent: bool,
) -> (CollectionDescription, ActivityBlockingFocusResult, Vec<ActivityObservation>) {
    let mut waiter = frames::request("42");
    waiter.statement_identity = Some(NativeIdentity::new("original-statement"));
    waiter.waiting_on_lock = Some(true);
    waiter.wait = Some(ActivityWait {
        class: None,
        identified_resources: vec![],
        native_type: Some("Lock: relation".into()),
        elapsed_ms: Some("1000".into()),
        owners: vec![],
        resources: None,
    });
    let partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("native-lineage"),
            native: NativeIdentity::new("native-member"),
        }),
        epoch: Some("original-native-epoch".into()),
    };
    let mut original_source = frames::source(1);
    if qualified_parent {
        original_source.partition = partition.clone();
    }
    let reference = frames::references(&original_source, std::slice::from_ref(&waiter)).remove(0);
    let original = original_source.observation.activity().unwrap();
    let focus = ActivityRequestFocus {
        parent: reference,
        target: original_source.target.clone(),
        partition: original_source.partition.clone(),
        database_writable: original.database_writable,
        source: original_source.source.clone(),
        epoch: original.epoch.clone().unwrap(),
        original_observed_at: original.at.clone(),
        statement_identity: waiter.statement_identity.clone().unwrap(),
        original: waiter.clone(),
    };
    assert!(focus.is_blocking_focus());
    let mut owner = frames::request("43");
    owner.key.request = None;
    owner.key.transaction = Some("native-transaction-start".into());
    owner.idle = true;
    owner.state = "idle in transaction".into();
    owner.open_transactions = Some(1);
    owner.elapsed_ms = None;
    owner.request_counters = None;
    owner.scope = ActivityScopeBasis::SessionDatabase;
    waiter.wait.as_mut().unwrap().owners = vec![
        ActivityWaitOwner::Session { id: "43".into(), generation: Some(owner.key.session.generation.clone()) },
        ActivityWaitOwner::Transaction { native_id: "0".into(), owner: TransactionWaitOwner::Prepared },
    ];
    let rows = vec![waiter, owner];
    let result = ActivityBlockingFocusResult {
        focus,
        binding: if qualified_parent {
            FocusedRequestBinding::Matched
        } else {
            FocusedRequestBinding::UnverifiedOriginalScope
        },
        membership: FocusedRequestMembership::Observed,
        request_end: BlockingRequestState::SameStatement,
        closure: BlockingClosureDescription {
            status: BlockingClosureStatus::Observed,
            max_lookups: MAX_BLOCKING_LOOKUPS,
            max_depth: MAX_BLOCKING_DEPTH,
            max_participants: MAX_BLOCKING_PARTICIPANTS,
        },
    };
    let source = CollectionDescription {
        target: original_source.target,
        partition,
        source: "focused-blocking".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedBlocking {
            snapshot: ActivitySnapshotDescription {
                epoch: Some("original-native-epoch".into()),
                database_writable: Some(true),
                observed_from: "2026-11-13T11:00:00Z".into(),
                observed_until: "2026-11-13T11:00:00.2Z".into(),
                at: "2026-11-13T11:00:00.1Z".into(),
                scope: MeasurementScope::Database,
                population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
                native_rows: rows.len() as u32,
                observations: rows.len() as u32,
                coverage: StateSourceCoverage::observed("focused-blocking".into(), rows.len() as u32, false),
            },
            result: Box::new(result.clone()),
        },
    };
    assert!(source.partition_is_consistent());
    (source, result, rows)
}

fn borrowed(rows: &[ActivityObservation]) -> Vec<ActivityFrameRow<'_>> {
    rows.iter().map(|observation| ActivityFrameRow { observation, reference: None }).collect()
}

#[test]
fn qualified_native_frame_preserves_every_owner_and_unresolved_prepared_transaction() {
    let (source, result, rows) = fixture(true);
    let frame = ActivityFrameEvidence::new(&source, &borrowed(&rows), true).unwrap();
    assert!(result.accepts_frame(&frame));
    let edges = ActivityEvidence::blocking_observations(&rows).unwrap();
    assert_eq!(edges.len(), 2);
    assert!(edges.iter().any(|edge| edge.owner_observed));
    assert!(edges.iter().any(|edge| !edge.owner_observed && matches!(&edge.owner, ActivityWaitOwner::Transaction { native_id, owner: TransactionWaitOwner::Prepared } if native_id == "0")));
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::FocusedBlocking { focus: Box::new(result.focus.clone()) },
        budget: CollectionBudget::default(),
    };
    assert!(source.matches_request(&request));
    assert!(!source.matches_request(&CollectionRequest { selection: CollectionSelection::Activity, ..request }));
    for row in &rows {
        assert!(
            ExportedRecord::new(
                &source,
                &CaptureIdentity { observation_id: "captured-blocking".into(), revision: None },
                row.clone().into()
            )
            .is_ok()
        );
    }
}

#[test]
fn current_native_authority_does_not_upgrade_the_original_target_namespace() {
    let (source, result, rows) = fixture(false);
    let frame = ActivityFrameEvidence::new(&source, &borrowed(&rows), true).unwrap();
    assert!(result.accepts_frame(&frame));
    assert_eq!(result.focus.partition, SourcePartition::Target);
    assert_eq!(result.binding, FocusedRequestBinding::UnverifiedOriginalScope);
    let mut changed = source.clone();
    let SourceObservation::FocusedBlocking { result, .. } = &mut changed.observation else { unreachable!() };
    result.binding = FocusedRequestBinding::Matched;
    assert!(!changed.partition_is_consistent());
}

#[test]
fn unrelated_actors_missing_waiters_and_statement_changes_cannot_become_a_complete_closure() {
    let (source, result, mut rows) = fixture(true);
    let frame = ActivityFrameEvidence::new(&source, &borrowed(&rows[1..]), false).unwrap();
    assert!(
        result.accepts_frame(&frame),
        "warehouse omissions preserve the original header, not a healthy empty graph"
    );
    let mut unrelated = source.clone();
    rows.push(frames::request("44"));
    let SourceObservation::FocusedBlocking { snapshot, .. } = &mut unrelated.observation else { unreachable!() };
    snapshot.native_rows += 1;
    snapshot.observations += 1;
    snapshot.coverage.records += 1;
    let frame = ActivityFrameEvidence::new(&unrelated, &borrowed(&rows), true).unwrap();
    assert!(!result.accepts_frame(&frame));
    rows[0].statement_identity = Some(NativeIdentity::new("another-statement"));
    assert!(!result.accepts_row(&rows[0]));
}

#[test]
fn role_member_epoch_and_source_bounds_are_not_repaired_by_a_current_frame() {
    let (source, _, rows) = fixture(true);
    for field in ["role", "member", "epoch", "clock", "coverage"] {
        let mut changed = source.clone();
        match field {
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition else {
                    unreachable!()
                };
                member.native.id = "other-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { unreachable!() };
                *epoch = Some("new-process".into());
            }
            _ => {
                let SourceObservation::FocusedBlocking { snapshot, .. } = &mut changed.observation else {
                    unreachable!()
                };
                match field {
                    "role" => snapshot.database_writable = Some(false),
                    "clock" => snapshot.observed_from = "2026-11-13T09:00:00Z".into(),
                    _ => snapshot.coverage.complete_scope = true,
                }
            }
        }
        assert!(ActivityFrameEvidence::new(&changed, &borrowed(&rows), true).is_err(), "{field}");
    }
}

#[test]
fn final_request_disappearance_preserves_the_observed_frame_without_claiming_incident_recovery() {
    let (mut source, _, rows) = fixture(true);
    for state in [BlockingRequestState::Changed, BlockingRequestState::Missing] {
        let SourceObservation::FocusedBlocking { result, .. } = &mut source.observation else { unreachable!() };
        result.request_end = state;
        let result = result.as_ref().clone();
        let frame = ActivityFrameEvidence::new(&source, &borrowed(&rows), true).unwrap();
        assert!(result.accepts_frame(&frame));
        assert_eq!(result.membership, FocusedRequestMembership::Observed);
        assert_eq!(ActivityEvidence::blocking_observations(&rows).unwrap().len(), 2);
    }
}

fn original_checkpoint(focus: &ActivityRequestFocus) -> (CollectionDescription, DiagnosticCheckpoint) {
    let mut original = frames::source(1);
    original.partition = focus.partition.clone();
    let scope = DiagnosticScope::from_source(&original).unwrap();
    assert!(focus.matches_parent(&original, &focus.original, &focus.parent));
    let episode = DiagnosticEpisode::start(
        scope,
        DiagnosticSignal::LockWait { key: focus.original.key.clone() },
        focus.original_observed_at.clone(),
        focus.parent.clone(),
    )
    .unwrap();
    let checkpoint = DiagnosticCheckpoint {
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        target: original.target.clone(),
        input: focus.parent.capture_ref(),
        episodes: vec![episode],
    };
    assert!(checkpoint.is_valid());
    (original, checkpoint)
}

#[test]
fn native_lock_wait_without_a_duration_plans_one_question_and_a_timed_symptom_does_not_duplicate_it() {
    let (_, result, _) = fixture(true);
    let mut source = frames::source(1);
    source.partition = result.focus.partition;
    for duration in [None, Some("40000")] {
        let mut row = result.focus.original.clone();
        row.elapsed_ms = Some("40000".into());
        row.blocking_status = ActivityBlockingStatus::Unknown;
        row.wait.as_mut().unwrap().elapsed_ms = duration.map(str::to_owned);
        let refs = frames::references(&source, std::slice::from_ref(&row));
        let update = observe_diagnostic_activity(
            &source,
            &refs[0].capture_ref(),
            &[DiagnosticActivityRow { reference: &refs[0], observation: &row }],
            None,
            &ActivityPolicy::default(),
            "2026-11-13T10:00:02Z",
        )
        .unwrap();
        let native = update
            .checkpoint
            .episodes
            .iter()
            .find(|episode| matches!(episode.signal, DiagnosticSignal::LockWait { .. }))
            .unwrap();
        assert_eq!(update.focuses.len(), 1, "native and timed symptoms must not open duplicate native questions");
        assert_eq!(update.focuses[0].0, native.id);
        let DiagnosticFocus::Blocking { focus } = &update.focuses[0].1 else { panic!("native blocking question") };
        assert!(focus.matches_parent(&source, &row, &refs[0]));
        let timed = update
            .checkpoint
            .episodes
            .iter()
            .filter(|episode| {
                matches!(episode.signal, DiagnosticSignal::Activity { condition: ActivityCondition::Blocked, .. })
            })
            .count();
        assert_eq!(timed, usize::from(duration.is_some()));
    }
}

fn current_references(source: &CollectionDescription, rows: &[ActivityObservation]) -> Vec<DiagnosticEvidenceRef> {
    let capture = CaptureIdentity {
        observation_id: "later-focused-blocking-native-read".into(),
        revision: Some(CaptureRevision {
            outbox_id: digest(b"controlled blocking reader outbox"),
            owner_epoch: "1".into(),
            sequence: "2".into(),
        }),
    };
    rows.iter()
        .enumerate()
        .map(|(ordinal, row)| {
            let exported = ExportedRecord::new(source, &capture, row.clone().into()).unwrap();
            DiagnosticEvidenceRef::from_export(
                source,
                &digest(b"later-focused-blocking-manifest"),
                &exported,
                ordinal as u32,
            )
            .unwrap()
        })
        .collect()
}

#[test]
fn a_genuinely_qualified_original_plans_one_question_and_retains_every_explanation_receipt() {
    let (source, result, mut rows) = fixture(true);
    rows[0].wait.as_mut().unwrap().owners.retain(|owner| matches!(owner, ActivityWaitOwner::Session { .. }));
    let (original, checkpoint) = original_checkpoint(&result.focus);
    let planned = checkpoint.episodes[0].plan_focus(&original, &result.focus.original, &result.focus.parent).unwrap();
    let DiagnosticFocusPlan::Ready(question) = planned else {
        panic!("qualified original asks a native lock question")
    };
    assert_eq!(*question, DiagnosticFocus::Blocking { focus: Box::new(result.focus.clone()) });
    assert_eq!(DiagnosticFocus::from_selection(&question.activity_selection().unwrap()), Some(*question));

    let references = current_references(&source, &rows);
    let observed = rows
        .iter()
        .zip(&references)
        .map(|(observation, reference)| DiagnosticActivityRow { reference, observation })
        .collect::<Vec<_>>();
    let update = analyze_diagnostic_blocking(
        &source,
        &references[0].capture_ref(),
        &observed,
        &checkpoint,
        "2026-11-13T11:00:01Z",
        None,
    )
    .unwrap();
    assert_eq!(update.revisions.len(), 1);
    let episode = &update.checkpoint.episodes[0];
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Analyzed);
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert!(episode.recovery.is_none());
    assert_eq!(episode.first, result.focus.parent);
    let report = episode.report.as_ref().unwrap();
    assert!(report.matches_blocking_input(episode, &source, references.iter().zip(&rows)));
    assert_eq!(report.references().count(), 2);
    assert!(update.revisions[0].inputs.contains(&result.focus.parent));
    assert!(references.iter().all(|reference| update.revisions[0].inputs.contains(reference)));
    assert!(report.matches_observation(
        episode,
        &source,
        &ExportedRecord::new(&source, &references[0].capture, rows[0].clone().into()).unwrap()
    ));
}

#[test]
fn unverified_original_scope_never_becomes_a_matched_incident_through_later_current_owner_authority() {
    let (source, result, rows) = fixture(false);
    let (original, checkpoint) = original_checkpoint(&result.focus);
    assert!(matches!(
        checkpoint.episodes[0].plan_focus(&original, &result.focus.original, &result.focus.parent).unwrap(),
        DiagnosticFocusPlan::Ready(_)
    ));
    let references = current_references(&source, &rows);
    let observed = rows
        .iter()
        .zip(&references)
        .map(|(observation, reference)| DiagnosticActivityRow { reference, observation })
        .collect::<Vec<_>>();
    let update = analyze_diagnostic_blocking(
        &source,
        &references[0].capture_ref(),
        &observed,
        &checkpoint,
        "2026-11-13T11:00:01Z",
        None,
    )
    .unwrap();
    let episode = &update.checkpoint.episodes[0];
    assert_eq!(episode.scope, checkpoint.episodes[0].scope);
    assert_eq!(episode.first, result.focus.parent);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Partial);
    assert!(episode.report.is_none() && episode.recovery.is_none());
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(
        update.checkpoint.episodes.len(),
        1,
        "no automatic current-frame rebaseline creates a matched replacement"
    );
}

#[test]
fn final_disappearance_and_unknown_owners_preserve_owed_history_without_recovery_or_borrowed_context() {
    let (mut source, focus_result, rows) = fixture(true);
    let (_, checkpoint) = original_checkpoint(&focus_result.focus);
    let SourceObservation::FocusedBlocking { result, .. } = &mut source.observation else { unreachable!() };
    result.request_end = BlockingRequestState::Missing;
    let references = current_references(&source, &rows);
    let observed = rows
        .iter()
        .zip(&references)
        .map(|(observation, reference)| DiagnosticActivityRow { reference, observation })
        .collect::<Vec<_>>();
    let update = analyze_diagnostic_blocking(
        &source,
        &references[0].capture_ref(),
        &observed,
        &checkpoint,
        "2026-11-13T11:00:01Z",
        None,
    )
    .unwrap();
    let episode = &update.checkpoint.episodes[0];
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Partial);
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert!(episode.recovery.is_none());
    assert!(episode.report.as_ref().unwrap().matches_blocking_input(episode, &source, references.iter().zip(&rows)));
    let graph =
        crate::blocking::analyze_activity(&ActivityFrameEvidence::new(&source, &borrowed(&rows), true).unwrap())
            .unwrap()
            .unwrap();
    let prepared = graph
        .chains
        .iter()
        .find(|chain| {
            matches!(
                chain.tree.owner,
                Some(ActivityWaitOwner::Transaction { owner: TransactionWaitOwner::Prepared, .. })
            )
        })
        .unwrap();
    assert!(prepared.tree.context.is_none());
    assert_eq!(prepared.tree.session, "0");
}

#[test]
fn typed_graph_keys_survive_decoration_when_known_and_unknown_owners_share_a_display_pid() {
    let (source, result, mut rows) = fixture(true);
    rows[0].wait.as_mut().unwrap().owners.retain(|owner| matches!(owner, ActivityWaitOwner::Session { .. }));
    rows[1].key.request = rows[0].key.request.clone();
    rows[1].key.transaction = None;
    rows[1].idle = false;
    rows[1].request_counters = Some(ActivityCounters::default());
    rows[1].statement_identity = Some(NativeIdentity::new("current-owner-request"));
    rows[1].state = "running".into();
    rows[1].waiting_on_lock = Some(true);
    rows[1].wait = Some(ActivityWait {
        class: None,
        identified_resources: vec![],
        native_type: Some("Lock: relation".into()),
        elapsed_ms: None,
        resources: None,
        owners: vec![ActivityWaitOwner::Session { id: "43".into(), generation: None }],
    });
    let frame = ActivityFrameEvidence::new(&source, &borrowed(&rows), true).unwrap();
    assert!(result.accepts_frame(&frame));
    let graph = crate::blocking::analyze_activity(&frame).unwrap().unwrap();
    let [chain] = graph.chains.as_slice() else { panic!("one native chain through an unresolved generation") };
    assert_eq!(chain.tree.session, "43");
    assert_eq!(chain.tree.children[0].session, "43");
    assert_ne!(chain.tree.key, chain.tree.children[0].key);
    assert_ne!(chain.tree.owner, chain.tree.children[0].owner);
    assert!(chain.tree.context.is_none());
    assert!(chain.tree.children[0].context.is_some());
    assert_eq!(chain.waiters, 2);
    assert_eq!(graph.waiting, 2);
    assert!(!result.has_complete_owner_context(&frame));
}

#[test]
fn a_same_session_relationship_keeps_both_requests_but_never_selects_an_owner_request_or_completes_analysis() {
    let (mut source, _, mut rows) = fixture(true);
    let waiter = rows[0].key.session.clone();
    rows[0].wait.as_mut().unwrap().owners =
        vec![ActivityWaitOwner::Session { id: waiter.id, generation: Some(waiter.generation) }];
    let mut peer = rows[0].clone();
    peer.key.request.as_mut().unwrap().id = "other-request".into();
    peer.statement_identity = Some(NativeIdentity::new("other-native-request-statement"));
    peer.wait = None;
    peer.waiting_on_lock = None;
    rows[1] = peer;
    let SourceObservation::FocusedBlocking { result, .. } = &mut source.observation else { unreachable!() };
    let (_, checkpoint) = original_checkpoint(&result.focus);
    let result = result.as_ref().clone();
    let frame = ActivityFrameEvidence::new(&source, &borrowed(&rows), true).unwrap();
    assert!(result.accepts_frame(&frame));
    let relationships = ActivityEvidence::blocking_observations(&rows).unwrap();
    assert_eq!(relationships.len(), 1);
    assert!(relationships[0].reports_waiter_session_id());
    assert!(relationships[0].owner_observed);
    assert!(!result.has_complete_owner_context(&frame));
    assert!(crate::blocking::analyze_activity(&frame).unwrap().is_none(), "no invented cross-request owner chain");
    let references = current_references(&source, &rows);
    let observed = rows
        .iter()
        .zip(&references)
        .map(|(observation, reference)| DiagnosticActivityRow { reference, observation })
        .collect::<Vec<_>>();
    let update = analyze_diagnostic_blocking(
        &source,
        &references[0].capture_ref(),
        &observed,
        &checkpoint,
        "2026-11-13T11:00:01Z",
        None,
    )
    .unwrap();
    assert_eq!(update.checkpoint.episodes[0].analysis, DiagnosticAnalysisStatus::Partial);
    assert!(
        update.checkpoint.episodes[0].report.is_some(),
        "the authentic raw relationship remains explained as unresolved"
    );
}
