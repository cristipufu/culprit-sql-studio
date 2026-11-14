use super::*;

fn partition(member: Option<&str>, epoch: Option<&str>) -> SourcePartition {
    SourcePartition::InstanceLocal {
        member: member.map(|member| SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Node,
            namespace: NativeIdentity::new("logical-db-native-id"),
            native: NativeIdentity::new(member),
        }),
        epoch: epoch.map(str::to_owned),
    }
}

fn local_frame(
    sequence: u64,
    partition: SourcePartition,
    rows: Vec<ActivityObservation>,
) -> (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ActivityObservation)>) {
    let qualified = partition.is_qualified();
    let mut frame = frame(sequence, rows, qualified);
    let SourceObservation::Activity(native) = &mut frame.0.observation else { panic!() };
    native.epoch = partition.native_epoch().map(str::to_owned);
    native.database_writable = Some(true);
    frame.0.partition = partition;
    frame.1.source_scope_id = frame.0.captured_range().unwrap().scope_id;
    for (ordinal, (reference, row)) in frame.2.iter_mut().enumerate() {
        let exported = ExportedRecord::new(&frame.0, &frame.1.capture, TelemetryRecord::from(row.clone())).unwrap();
        *reference =
            DiagnosticEvidenceRef::from_export(&frame.0, &frame.1.manifest_sha256, &exported, ordinal as u32).unwrap();
    }
    frame
}

#[test]
fn another_replica_cannot_recover_or_revise_the_original_members_episode() {
    let original = observe(&local_frame(1, partition(Some("member-a"), Some("start")), vec![actor()]), None);
    let empty_other = local_frame(2, partition(Some("member-b"), Some("start")), vec![]);
    let independent = observe(&empty_other, Some(&original.checkpoint));
    assert!(independent.revisions.is_empty());
    assert_eq!(independent.checkpoint.episodes, original.checkpoint.episodes);
    let same_member = local_frame(3, partition(Some("member-a"), Some("start")), vec![]);
    let recovered = observe(&same_member, Some(&independent.checkpoint));
    assert!(recovered.revisions.iter().any(|revision| revision.episode.lifecycle == DiagnosticLifecycle::Resolved));
}

#[test]
fn restarted_member_makes_only_its_own_old_generation_uncertain() {
    let first = observe(&local_frame(1, partition(Some("member-a"), Some("start-a")), vec![actor()]), None);
    let other =
        observe(&local_frame(2, partition(Some("member-b"), Some("start-b")), vec![actor()]), Some(&first.checkpoint));
    let restarted =
        observe(&local_frame(3, partition(Some("member-a"), Some("new-start")), vec![]), Some(&other.checkpoint));
    assert_eq!(restarted.revisions.len(), 1);
    let affected = &restarted.revisions[0].episode;
    assert_eq!(affected.id, first.checkpoint.episodes[0].id);
    assert_eq!(affected.lifecycle, DiagnosticLifecycle::Uncertain);
    assert!(affected.recovery.is_none());
    let member_b = other.checkpoint.episodes.iter().find(|episode| episode.id != affected.id).unwrap();
    assert!(restarted.checkpoint.episodes.contains(member_b));
}

#[test]
fn repeated_unknown_local_positive_points_keep_checkpoint_progress_without_unresolvable_episode_growth() {
    let known = observe(&local_frame(1, partition(Some("known-member"), Some("start")), vec![actor()]), None);
    let mut checkpoint = known.checkpoint;
    for sequence in 2..32 {
        let capture = local_frame(sequence, partition(None, None), vec![actor()]);
        let original = capture.2[0].1.clone();
        let update = observe(&capture, Some(&checkpoint));
        assert!(update.revisions.is_empty());
        assert!(update.focuses.is_empty());
        assert_eq!(update.checkpoint.episodes, checkpoint.episodes);
        assert_eq!(update.checkpoint.input, capture.1);
        assert_ne!(update.checkpoint.input, checkpoint.input);
        assert_eq!(capture.2[0].1, original, "the original positive native point remains intact");
        checkpoint = update.checkpoint;
    }
    assert_eq!(checkpoint.episodes.len(), 1, "unknown points cannot fill the bounded working set");
}

#[test]
fn native_focus_binds_member_generation_and_original_role_without_making_role_a_member_identity() {
    let original = local_frame(1, partition(Some("member-a"), Some("start")), vec![actor()]);
    let update = observe(&original, None);
    let focus = &update.focuses[0].1;
    assert_eq!(focus.partition, original.0.partition);
    assert_eq!(focus.database_writable, Some(true));
    assert!(focus.matches_parent(&original.0, &original.2[0].1, &original.2[0].0));
    let mut role_changed = original.0.clone();
    let SourceObservation::Activity(native) = &mut role_changed.observation else { panic!() };
    native.database_writable = Some(false);
    assert_eq!(original.0.captured_range().unwrap().scope_id, role_changed.captured_range().unwrap().scope_id);
    assert!(!focus.matches_parent(&role_changed, &original.2[0].1, &original.2[0].0));
    let other = local_frame(1, partition(Some("member-b"), Some("start")), vec![actor()]);
    assert!(!focus.matches_parent(&other.0, &other.2[0].1, &other.2[0].0));
    let restarted = local_frame(1, partition(Some("member-a"), Some("new-start")), vec![actor()]);
    assert!(!focus.matches_parent(&restarted.0, &restarted.2[0].1, &restarted.2[0].0));
}
