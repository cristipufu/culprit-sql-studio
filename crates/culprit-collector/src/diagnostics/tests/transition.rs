use super::*;

#[tokio::test]
async fn admission_revisions_keep_incident_history_and_do_not_replay_native_collection() {
    let mut harness = Harness::new().await;
    let native = harness.capture(baseline(0, vec![participant()])).await;
    assert_eq!(harness.work(1).await, 1);
    let original = harness.checkpoint().await.episodes.remove(0);
    let native_calls = harness.calls.counts();
    set_analysis(&mut harness.outbox, &original.latest, &original.id, DiagnosticAnalysisStatus::Collecting, None)
        .await
        .unwrap();
    let collecting = harness.checkpoint().await.episodes.remove(0);
    assert_eq!(collecting.revision, "2");
    assert_eq!(collecting.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(collecting.analysis, DiagnosticAnalysisStatus::Collecting);
    assert_eq!(collecting.first, original.first);
    assert_eq!(collecting.latest, original.latest);
    assert!(harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    let captures = harness.outbox.status(50, None).await.unwrap().entries.len();
    set_analysis(&mut harness.outbox, &original.latest, &original.id, DiagnosticAnalysisStatus::Collecting, None)
        .await
        .unwrap();
    assert_eq!(
        harness.outbox.status(50, None).await.unwrap().entries.len(),
        captures,
        "an unchanged admission decision cannot create another derived capture"
    );
    set_analysis(
        &mut harness.outbox,
        &original.latest,
        &original.id,
        DiagnosticAnalysisStatus::Deferred,
        Some(DiagnosticAnalysisIssue::BudgetExhausted),
    )
    .await
    .unwrap();
    let deferred = harness.checkpoint().await.episodes.remove(0);
    assert_eq!(deferred.revision, "3");
    assert_eq!(deferred.issue, Some(DiagnosticAnalysisIssue::BudgetExhausted));
    assert_eq!(deferred.first, original.first);
    assert_eq!(harness.calls.counts(), native_calls);
    assert_eq!(harness.outbox.verify(&native.observation_id).await.unwrap().capture, native);
}

#[tokio::test]
async fn a_forged_reference_with_a_real_capture_id_cannot_change_analysis() {
    let mut harness = Harness::new().await;
    harness.capture(baseline(0, vec![participant()])).await;
    harness.work(1).await;
    let before = harness.checkpoint().await;
    let episode = &before.episodes[0];
    let mut forged = episode.latest.clone();
    forged.payload_sha256 = digest(b"another row");
    assert!(forged.is_valid());
    let error = set_analysis(&mut harness.outbox, &forged, &episode.id, DiagnosticAnalysisStatus::Collecting, None)
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    assert_eq!(harness.checkpoint().await, before);
}

#[tokio::test]
async fn another_actor_in_the_same_capture_cannot_supply_an_episodes_admission_evidence() {
    let mut harness = Harness::new().await;
    let first = participant();
    let mut second = first.clone();
    second.key.session.id = "other-session".into();
    second.key.request.as_mut().unwrap().id = "other-request".into();
    assert!(second.is_valid());
    harness.capture(baseline(0, vec![first, second])).await;
    harness.work(1).await;
    let before = harness.checkpoint().await;
    assert_eq!(before.episodes.len(), 2);
    let error = set_analysis(
        &mut harness.outbox,
        &before.episodes[1].latest,
        &before.episodes[0].id,
        DiagnosticAnalysisStatus::Collecting,
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::InvalidPage))
            | Some(ErrorDetail::Delivery(DeliveryProblem::Integrity))
    ));
    assert_eq!(harness.checkpoint().await, before);
}
