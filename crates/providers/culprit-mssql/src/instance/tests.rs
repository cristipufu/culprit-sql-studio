use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};

use super::test_support::{epoch, metadata};

#[tokio::test]
async fn instance_identity_ignores_the_member_database_and_keeps_process_lifetime_separate() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![replace(&metadata(), &[("process_id", Val::Int(43))])]),
        Ok(vec![epoch(200)]),
    ]);
    let (first, at) = read(&mut reader).await.unwrap();
    let (next, _) = read(&mut reader).await.unwrap();
    assert_eq!(at, "2020-01-01T06:00:00.000000000Z");
    assert_eq!(first.identity, next.identity);
    assert!(first != next);
    assert_eq!(first.identity.kind, SharedResourceKind::Instance);
    assert_eq!(first.identity.native.id, "fixture-machine\\NATIVE");
    assert!(reader.calls.iter().all(|call| call.limit == 2 && call.sql.contains("/* culprit */")));
    assert!(reader.calls.iter().all(|call| !call.sql.contains("sys.databases")));
    assert_eq!(reader.closed, 0, "a borrowed identity reader never claims session shutdown");
}

#[tokio::test]
async fn unsupported_platforms_and_incomplete_identity_stop_before_instance_statistics() {
    for changes in [
        vec![("edition", Val::Int(5))],
        vec![("edition", Val::Int(11))],
        vec![("major", Val::Text("10".into()))],
        vec![("process_id", Val::Int(0))],
        vec![("machine_name", Val::Null)],
        vec![("server_name", Val::Text("".into()))],
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&metadata(), &changes)])]);
        assert!(read(&mut reader).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
    let mut reader = FakeRead::new(vec![Ok(vec![metadata()]), Ok(vec![epoch(501)])]);
    assert!(read(&mut reader).await.is_err());
    let mut reader = FakeRead::new(vec![
        Ok(vec![metadata()]),
        Err(Error::new(ErrorCode::PermissionDenied, "fixture denied").into()),
    ]);
    assert_eq!(read(&mut reader).await.err().unwrap().code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn managed_instance_virtual_major_versions_do_not_fall_into_the_azure_database_route() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![replace(&metadata(), &[("edition", Val::Int(8)), ("major", Val::Text("12".into()))])]),
        Ok(vec![epoch(100)]),
    ]);
    let (instance, _) = read(&mut reader).await.unwrap();
    assert_eq!(instance.identity.kind, SharedResourceKind::Instance);
    assert_eq!(instance.major, 12);
    assert_eq!(reader.calls.len(), 2);
    assert!(!reader.calls.iter().any(|call| call.sql.contains("sys.dm_user_db_resource_governance")));
}
