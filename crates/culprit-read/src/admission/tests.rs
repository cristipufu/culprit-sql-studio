use super::*;

const FIRST: &str = "0123456789abcdef0123456789abcdef";
const SECOND: &str = "1123456789abcdef0123456789abcdef";

#[tokio::test]
async fn dropping_unsettled_work_preserves_debt_and_requires_the_exact_observation() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("admission");
    let guard = SourceAdmission::acquire(root.clone()).await.unwrap();
    guard.started(FIRST.into(), SourceAdmissionRoute::SavedConnection { name: "production".into() }).await.unwrap();
    assert!(
        guard.started(SECOND.into(), SourceAdmissionRoute::SavedConnection { name: "another".into() }).await.is_err()
    );
    drop(guard);
    assert!(SourceAdmission::acquire(root.clone()).await.is_err());
    let pending = SourceAdmission::inspect(root.clone()).await.unwrap();
    assert_eq!(pending.observation_id.as_deref(), Some(FIRST));
    assert_eq!(pending.route.as_ref().and_then(SourceAdmissionRoute::saved_connection), Some("production"));
    assert!(SourceAdmission::acknowledge(root.clone(), SECOND).await.is_err());
    assert!(SourceAdmission::inspect(root.clone()).await.unwrap().source_quiescence_required);
    assert!(!SourceAdmission::acknowledge(root.clone(), FIRST).await.unwrap().source_quiescence_required);
    assert!(SourceAdmission::acquire(root).await.is_ok());
}

#[tokio::test]
async fn a_live_owner_prevents_replacement_and_operator_acknowledgement() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("admission");
    let owner = SourceAdmission::acquire(root.clone()).await.unwrap();
    owner.started(FIRST.into(), SourceAdmissionRoute::SavedConnection { name: "production".into() }).await.unwrap();
    assert!(SourceAdmission::acquire(root.clone()).await.is_err());
    assert!(SourceAdmission::acknowledge(root.clone(), FIRST).await.is_err());
    owner.confirmed_quiet().await.unwrap();
    assert!(SourceAdmission::acquire(root.clone()).await.is_err());
    drop(owner);
    assert!(SourceAdmission::acquire(root).await.is_ok());
}

#[tokio::test]
async fn corrupt_or_unsupported_safety_state_never_admits_new_work() {
    let cases = [
        b"".to_vec(),
        b"not-json".to_vec(),
        serde_json::to_vec(&serde_json::json!({"format":FORMAT+1,"work":{"state":"quiet"}})).unwrap(),
        br#"{"format":1,"work":{"state":"started","observation_id":"invalid","connection":"production"}}"#.to_vec(),
        vec![b' '; MAX_STATE_BYTES + 1],
    ];
    for bytes in cases {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("admission");
        drop(SourceAdmission::acquire(root.clone()).await.unwrap());
        fs::write(root.join(STATE_FILE), bytes).unwrap();
        assert!(SourceAdmission::acquire(root.clone()).await.is_err());
        assert!(SourceAdmission::inspect(root).await.is_err());
    }
}

#[tokio::test]
async fn an_abandoned_temporary_cannot_erase_or_replace_committed_debt() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("admission");
    let owner = SourceAdmission::acquire(root.clone()).await.unwrap();
    owner.started(FIRST.into(), SourceAdmissionRoute::SavedConnection { name: "production".into() }).await.unwrap();
    drop(owner);
    fs::write(root.join(format!(".{STATE_FILE}.new")), br#"{"format":1,"work":{"state":"quiet"}}"#).unwrap();
    assert!(SourceAdmission::acquire(root.clone()).await.is_err());
    assert_eq!(SourceAdmission::inspect(root.clone()).await.unwrap().observation_id.as_deref(), Some(FIRST));
    assert!(SourceAdmission::acknowledge(root.clone(), FIRST).await.is_ok());
    assert!(SourceAdmission::acquire(root).await.is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn dangling_state_symlinks_are_errors_rather_than_missing_state() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("admission");
    drop(SourceAdmission::acquire(root.clone()).await.unwrap());
    std::os::unix::fs::symlink(root.join("missing"), root.join(STATE_FILE)).unwrap();
    assert!(SourceAdmission::acquire(root.clone()).await.is_err());
    assert!(SourceAdmission::inspect(root).await.is_err());
}

#[tokio::test]
async fn failed_publication_does_not_claim_to_have_settled_work() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("admission");
    let owner = SourceAdmission::acquire(root.clone()).await.unwrap();
    owner.started(FIRST.into(), SourceAdmissionRoute::SavedConnection { name: "production".into() }).await.unwrap();
    fs::create_dir(root.join(format!(".{STATE_FILE}.new"))).unwrap();
    assert!(owner.confirmed_quiet().await.is_err());
    assert!(owner.status().await.source_quiescence_required);
    assert!(SourceAdmission::inspect(root).await.unwrap().source_quiescence_required);
}

#[tokio::test]
async fn unsaved_test_debt_retains_only_its_exact_invocation_route() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("admission");
    let owner = SourceAdmission::acquire(root.clone()).await.unwrap();
    let route = SourceAdmissionRoute::UnsavedConnectionTest { invocation_id: FIRST.into() };
    owner.started(FIRST.into(), route.clone()).await.unwrap();
    drop(owner);
    let status = SourceAdmission::inspect(root.clone()).await.unwrap();
    assert_eq!(status.route, Some(route));
    assert!(status.route.as_ref().unwrap().saved_connection().is_none());
    assert!(status.source_quiescence_required);
    assert!(SourceAdmission::acknowledge(root.clone(), SECOND).await.is_err());
    assert!(SourceAdmission::acquire(root.clone()).await.is_err());
    assert!(!SourceAdmission::acknowledge(root, FIRST).await.unwrap().source_quiescence_required);
}

#[tokio::test]
async fn contradictory_or_invalid_test_routes_cannot_start_or_replace_the_original_route() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("admission");
    let owner = SourceAdmission::acquire(root.clone()).await.unwrap();
    for route in [
        SourceAdmissionRoute::UnsavedConnectionTest { invocation_id: SECOND.into() },
        SourceAdmissionRoute::SavedConnection { name: "".into() },
        SourceAdmissionRoute::SavedConnectionTest {
            name: "private\nroute".into(),
            test: culprit_core::telemetry::ConnectionTestKind::Ssh,
        },
    ] {
        assert!(owner.started(FIRST.into(), route).await.is_err());
    }
    assert!(!owner.status().await.source_quiescence_required);
    let route = SourceAdmissionRoute::SavedConnectionTest {
        name: "production".into(),
        test: culprit_core::telemetry::ConnectionTestKind::Settings,
    };
    owner.started(FIRST.into(), route.clone()).await.unwrap();
    assert!(
        owner
            .started(SECOND.into(), SourceAdmissionRoute::SavedConnection { name: "production".into() })
            .await
            .is_err()
    );
    drop(owner);
    let retained = SourceAdmission::inspect(root.clone()).await.unwrap();
    assert_eq!(retained.route, Some(route));
    assert_eq!(retained.observation_id.as_deref(), Some(FIRST));
    assert!(retained.source_quiescence_required);
}
