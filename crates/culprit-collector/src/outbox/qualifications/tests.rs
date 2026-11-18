use super::*;
use crate::analytical::test_support::{QualificationFixture, QualificationFixtureCase};
use culprit_core::{ErrorDetail, RecipeQualificationState};

fn route() -> String {
    digest(b"controlled-saved-connection-route")
}
async fn qualify(outbox: &mut Outbox, fixture: &QualificationFixture) -> Result<RecipeQualificationSummary> {
    outbox
        .qualify_native(
            &SourceGate::default(),
            fixture,
            NativeQualificationRequest {
                connection: "owned-source",
                route_revision: &route(),
                recipe: &fixture.recipe,
                request: fixture.request(),
                cancellation: &CollectionCancellation::default(),
            },
        )
        .await
        .map(|outcome| outcome.summary)
}
async fn bindings(outbox: &Outbox) -> Vec<IssuedQualification> {
    outbox
        .delivery()
        .issued_qualifications(CompletedCaptureBudget::default(), &CollectionCancellation::default())
        .await
        .unwrap()
        .bindings
}
async fn owners(outbox: &Outbox, id: &str) -> Vec<String> {
    let id = id.to_owned();
    outbox
        .files
        .run(|| {
            move |files| {
                let mut statement = files
                    .journal
                    .conn
                    .prepare("SELECT owner FROM diagnostic_pins WHERE observation_id=?1 ORDER BY owner")
                    .map_err(storage)?;
                statement
                    .query_map([id], |row| row.get(0))
                    .map_err(storage)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(storage)
            }
        })
        .await
        .unwrap()
}
async fn reclaimable(outbox: &Outbox, id: &str) -> bool {
    let id = id.to_owned();
    outbox.files.run(|| move |files| files.eviction_candidate(&id)).await.unwrap().unwrap()
}
async fn fault(outbox: &Outbox, sql: &str) {
    let sql = sql.to_owned();
    outbox.files.run(|| move |files| files.journal.conn.execute_batch(&sql).map_err(storage)).await.unwrap();
}

#[tokio::test]
async fn trusted_live_issuance_is_atomic_pinned_and_restored_under_the_same_exclusive_owner() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, Default::default()).unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Approve);
    let summary = qualify(&mut outbox, &fixture).await.unwrap();
    assert_eq!(summary.state, RecipeQualificationState::Installed);
    assert_eq!(summary.source_queries, Some(2));
    assert_eq!((fixture.opens(), fixture.assessments(), fixture.closed()), (1, 1, true));
    let issued = bindings(&outbox).await.pop().unwrap();
    let id = issued.receipt().evidence.capture.observation_id.clone();
    assert_eq!(owners(&outbox, &id).await, [format!("qualification:{}", issued.id().unwrap())]);
    assert!(!reclaimable(&outbox, &id).await);
    assert!(Outbox::open(&path).is_err(), "another process/Files owner cannot mutate the live daemon journal");
    let delivery = outbox.delivery();
    drop(outbox);
    assert!(issued.is_active(), "delivery retains the actual exclusive owner");
    assert!(Outbox::open(&path).is_err());
    drop(delivery);
    assert!(!issued.is_active(), "closing the owner fences cached handles before unlocking");

    let mut restarted = Outbox::open(&path).unwrap();
    restarted.recover().await.unwrap();
    let restored = bindings(&restarted).await.pop().unwrap();
    assert_eq!(restored.id().unwrap(), issued.id().unwrap());
    assert!(restored.is_active());
    let original = restarted
        .delivery()
        .completed_snapshot_for_connection(
            restored.connection(),
            &DiagnosticAttemptReceipt {
                capture: restored.receipt().evidence.capture.clone(),
                manifest_sha256: Some(restored.receipt().evidence.manifest_sha256.clone()),
            },
            restored.dataset(),
            Default::default(),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(original.reference(0).unwrap(), restored.receipt().evidence);
    assert!(!reclaimable(&restarted, &id).await);
}

#[tokio::test]
async fn revoke_fences_an_already_verified_restore_before_publication_and_releases_only_its_own_pin() {
    let root = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Approve);
    qualify(&mut outbox, &fixture).await.unwrap();
    let verified_for_restore = bindings(&outbox).await.pop().unwrap();
    let id = verified_for_restore.receipt().evidence.capture.observation_id.clone();
    let pinned = id.clone();
    outbox
        .files
        .run(|| {
            move |files| {
                let tx = files.journal.conn.transaction().map_err(storage)?;
                crate::outbox::investigations::journal::pin(&tx, "independent-investigation", &pinned)?;
                tx.commit().map_err(storage)
            }
        })
        .await
        .unwrap();
    assert!(outbox.revoke_qualification("owned-source", &fixture.recipe.id).await.unwrap());
    assert!(!verified_for_restore.is_active(), "a later cache publish cannot resurrect the retired token");
    assert_eq!(owners(&outbox, &id).await, ["independent-investigation"]);
    assert!(!reclaimable(&outbox, &id).await);
    assert!(!outbox.revoke_qualification("owned-source", &fixture.recipe.id).await.unwrap());
    let revoked = bindings(&outbox).await.pop().unwrap();
    assert_eq!(revoked.summary().unwrap().state, RecipeQualificationState::Revoked);
    assert!(!revoked.is_active());
    fault(&outbox, "DELETE FROM diagnostic_pins WHERE owner='independent-investigation'").await;
    assert!(reclaimable(&outbox, &id).await);
    let new = qualify(&mut outbox, &fixture).await.unwrap();
    assert_ne!(new.approval_id, Some(verified_for_restore.id().unwrap()));
    assert!(!verified_for_restore.is_active());
    assert!(bindings(&outbox).await.pop().unwrap().revision() > revoked.revision());
}

#[tokio::test]
async fn replacing_or_revoking_cannot_publish_half_a_transaction_or_resurrect_an_old_live_token() {
    let root = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Approve);
    qualify(&mut outbox, &fixture).await.unwrap();
    let old = bindings(&outbox).await.pop().unwrap();
    let capture = old.receipt().evidence.capture.observation_id.clone();
    let pins = owners(&outbox, &capture).await;
    fault(&outbox, "CREATE TRIGGER stop_qualification BEFORE UPDATE ON qualifications BEGIN SELECT RAISE(ABORT,'controlled-stop'); END").await;
    assert!(qualify(&mut outbox, &fixture).await.is_err());
    assert!(!old.is_active(), "failed replacement conservatively retires its in-memory predecessor");
    assert_eq!(owners(&outbox, &capture).await, pins, "the original durable pin rolls back with the binding");
    let durable = bindings(&outbox).await.pop().unwrap();
    assert_eq!(durable.id().unwrap(), old.id().unwrap());
    assert!(outbox.revoke_qualification("owned-source", &fixture.recipe.id).await.is_err());
    assert!(!durable.is_active());
    assert_eq!(owners(&outbox, &capture).await, pins);
    fault(&outbox, "DROP TRIGGER stop_qualification").await;
    assert!(outbox.revoke_qualification("owned-source", &fixture.recipe.id).await.unwrap());
    assert!(owners(&outbox, &capture).await.is_empty());
}

#[tokio::test]
async fn imported_cost_json_or_an_untrusted_wrong_original_never_installs_an_approval() {
    for case in [
        QualificationFixtureCase::WrongDigest,
        QualificationFixtureCase::WrongOrdinal,
        QualificationFixtureCase::WrongBudget,
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
        let fixture = QualificationFixture::new(case);
        assert!(qualify(&mut outbox, &fixture).await.is_err());
        assert!(bindings(&outbox).await.is_empty());
        assert!(fixture.closed());
    }
    let root = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Approve);
    let request = fixture.request();
    let mut lease = outbox.reserve("owned-source", &request).await.unwrap();
    let identity = lease.identity().clone();
    let directory = lease.directory().to_owned();
    let manifest = capture::collect(
        &SourceGate::default(),
        &fixture,
        request,
        &directory,
        identity,
        &CollectionCancellation::default(),
        Some(&mut lease),
    )
    .await
    .unwrap();
    lease.finish().await.unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    assert!(
        bindings(&outbox).await.is_empty(),
        "sealing structurally identical native-cost JSON does not execute issuance"
    );
    assert_eq!(fixture.assessments(), 0);
}

#[tokio::test]
async fn refusal_cancel_and_unconfirmed_shutdown_keep_the_original_attempt_without_approval_or_debt_repair() {
    for case in
        [QualificationFixtureCase::Refuse, QualificationFixtureCase::Cancelled, QualificationFixtureCase::Unconfirmed]
    {
        let root = tempfile::tempdir().unwrap();
        let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
        let fixture = QualificationFixture::new(case);
        let result = qualify(&mut outbox, &fixture).await;
        if matches!(case, QualificationFixtureCase::Cancelled) {
            assert_eq!(CollectionStop::from_error(&result.unwrap_err()), CollectionStop::Cancelled);
        } else {
            let summary = result.unwrap();
            assert_eq!(summary.state, RecipeQualificationState::Refused);
            assert!(summary.approval_id.is_none());
        }
        assert!(fixture.closed());
        assert_eq!(fixture.assessments(), usize::from(matches!(case, QualificationFixtureCase::Refuse)));
        assert!(bindings(&outbox).await.is_empty());
        let status = outbox.status(10, None).await.unwrap();
        assert_eq!(status.captures, 1, "the original failed/refused attempt remains retained");
        assert_eq!(
            status.entries[0].source_work,
            if matches!(case, QualificationFixtureCase::Unconfirmed) {
                SourceWorkState::Unconfirmed
            } else {
                SourceWorkState::Quiescent
            }
        );
        let debts = status.source_debts;
        assert_eq!(debts, u32::from(matches!(case, QualificationFixtureCase::Unconfirmed)));
        outbox.revoke_qualification("owned-source", &fixture.recipe.id).await.unwrap();
        assert_eq!(outbox.status(10, None).await.unwrap().source_debts, debts);
    }
}

#[tokio::test]
async fn caller_cancellation_and_lookup_limits_refuse_before_source_creation_or_partial_restoration() {
    let root = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Approve);
    let cancelled = CollectionCancellation::default();
    cancelled.cancel();
    let error = outbox
        .qualify_native(
            &SourceGate::default(),
            &fixture,
            NativeQualificationRequest {
                connection: "owned-source",
                route_revision: &route(),
                recipe: &fixture.recipe,
                request: fixture.request(),
                cancellation: &cancelled,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::Cancelled);
    assert_eq!((fixture.opens(), fixture.assessments()), (0, 0));
    qualify(&mut outbox, &fixture).await.unwrap();
    let delivery = outbox.delivery();
    let error = delivery
        .issued_qualifications(
            CompletedCaptureBudget { max_bytes: 1, ..Default::default() },
            &CollectionCancellation::default(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert_eq!(
        CollectionStop::from_error(
            &delivery.issued_qualifications(Default::default(), &cancelled).await.err().unwrap()
        ),
        CollectionStop::Cancelled
    );
    assert_eq!(bindings(&outbox).await.len(), 1);
}

#[tokio::test]
async fn queued_metadata_lookup_observes_cancellation_and_deadline_without_starting_a_replacement_worker() {
    let root = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Approve);
    qualify(&mut outbox, &fixture).await.unwrap();
    let access = outbox.files.acquire().await;
    let delivery = outbox.delivery();
    let cancellation = CollectionCancellation::default();
    let reading = delivery.issued_qualifications(Default::default(), &cancellation);
    tokio::pin!(reading);
    tokio::select! {
        biased;
        result = &mut reading => panic!("queued read overtook its owner: {}",result.is_ok()),
        _ = tokio::time::sleep(std::time::Duration::from_millis(1)) => (),
    }
    cancellation.cancel();
    assert_eq!(CollectionStop::from_error(&reading.await.err().unwrap()), CollectionStop::Cancelled);
    let error = delivery
        .issued_qualifications(
            CompletedCaptureBudget { timeout: std::time::Duration::from_millis(1), ..Default::default() },
            &CollectionCancellation::default(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
    drop(access);
    assert_eq!(bindings(&outbox).await.len(), 1);
    assert_eq!(fixture.opens(), 1);
}
