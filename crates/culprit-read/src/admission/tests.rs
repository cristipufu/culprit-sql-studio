use super::*;

const FIRST: &str = "0123456789abcdef0123456789abcdef";
const SECOND: &str = "1123456789abcdef0123456789abcdef";

#[tokio::test]
async fn dropping_unsettled_work_preserves_debt_and_requires_the_exact_observation() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("admission");
    let guard = SourceAdmission::acquire(root.clone()).await.unwrap();
    guard.started(FIRST.into(), "production".into()).await.unwrap();
    assert!(guard.started(SECOND.into(), "another".into()).await.is_err());
    drop(guard);
    assert!(SourceAdmission::acquire(root.clone()).await.is_err());
    let pending = SourceAdmission::inspect(root.clone()).await.unwrap();
    assert_eq!(pending.observation_id.as_deref(), Some(FIRST));
    assert_eq!(pending.connection.as_deref(), Some("production"));
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
    owner.started(FIRST.into(), "production".into()).await.unwrap();
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
        br#"{"format":2,"work":{"state":"quiet"}}"#.to_vec(),
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
    owner.started(FIRST.into(), "production".into()).await.unwrap();
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
    owner.started(FIRST.into(), "production".into()).await.unwrap();
    fs::create_dir(root.join(format!(".{STATE_FILE}.new"))).unwrap();
    assert!(owner.confirmed_quiet().await.is_err());
    assert!(owner.status().await.source_quiescence_required);
    assert!(SourceAdmission::inspect(root).await.unwrap().source_quiescence_required);
}
