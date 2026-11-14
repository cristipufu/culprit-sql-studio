use super::*;
use crate::outbox::DiagnosticInputState;

fn member() -> SharedResourceIdentity {
    SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Node,
        namespace: NativeIdentity::new("native-database-namespace"),
        native: NativeIdentity::new("native-compute-member"),
    }
}

fn local_baseline(second: u32, partition: SourcePartition) -> Frame {
    let mut frame = baseline(second, vec![participant()]);
    let SourceObservation::Activity(native) = &mut frame.source.observation else { panic!() };
    native.epoch = partition.native_epoch().map(str::to_owned);
    native.database_writable = Some(true);
    native.coverage =
        StateSourceCoverage::observed(frame.source.source.clone(), native.observations, partition.is_qualified());
    frame.source.partition = partition;
    assert!(frame.source.partition_is_consistent());
    frame
}

#[tokio::test]
async fn repeated_unknown_local_points_preserve_raw_evidence_and_known_episodes_without_stalling_the_worker() {
    let mut harness = Harness::new().await;
    let qualified = SourcePartition::InstanceLocal { member: Some(member()), epoch: Some("native-start".into()) };
    harness.capture(local_baseline(0, qualified.clone())).await;
    assert_eq!(harness.work(1).await, 1);
    let known = harness.checkpoint().await.episodes;
    assert_eq!(known.len(), 1);

    let unknown = [
        SourcePartition::InstanceLocal { member: None, epoch: None },
        SourcePartition::InstanceLocal { member: Some(member()), epoch: None },
        SourcePartition::InstanceLocal { member: None, epoch: Some("native-start".into()) },
    ];
    for (ordinal, partition) in unknown.iter().cycle().take(6).enumerate() {
        let capture = harness.capture(local_baseline(10 + ordinal as u32 * 2, partition.clone())).await;
        let input =
            harness.outbox.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
        assert_eq!(input.rows.len(), 1, "missing authority must retain a readable positive native point");
        let native = input.source.observation.activity().unwrap();
        assert_eq!(native.coverage.state, culprit_core::SourceState::Partial);
        assert!(!native.coverage.complete_scope);
        assert!(!input.source.is_partition_qualified());

        assert_eq!(harness.work(1).await, 1);
        assert_eq!(
            harness.checkpoint().await.episodes,
            known,
            "unidentified points cannot expand or revise known episodes"
        );
        assert!(harness.revisions(&capture).await.is_empty());
        assert_eq!(
            harness.outbox.diagnostic_input_state(&input.parent, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
            DiagnosticInputState::Consumed
        );
        let raw = replay(&harness.path, &capture, &harness.root.path().join(format!("unknown-local-{ordinal}"))).await;
        let expected = input.rows.into_iter().map(|row| row.row).collect::<Vec<_>>();
        assert_eq!(raw, expected, "raw immutable evidence survives its no-episode analysis");
    }

    let later = harness.capture(local_baseline(40, qualified)).await;
    assert_eq!(harness.work(1).await, 1, "the next qualified input must still progress");
    let episodes = harness.checkpoint().await.episodes;
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].id, known[0].id);
    assert_eq!(episodes[0].first, known[0].first);
    assert_eq!(episodes[0].latest.capture, later);
    assert_eq!(episodes[0].lifecycle, DiagnosticLifecycle::Active);
    assert!(harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    assert_eq!(harness.calls.counts(), [8, 8, 8, 8]);
}
