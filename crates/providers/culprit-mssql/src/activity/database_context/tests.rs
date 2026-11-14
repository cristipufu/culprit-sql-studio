use super::*;
use crate::{
    activity::{ActivityScope, ReadOptions},
    instance::test_support::{epoch, metadata},
    resources::test_support::{FakeRead, replace},
    value::Val,
};

pub(crate) fn row(edition: i64) -> Rec {
    let base = crate::activity::tests::metadata(42);
    let mut values = base.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    values.extend([
        ("selected_database_id", Val::Int(7)),
        ("database_created_without_offset", Val::Text("2019-01-01T04:00:00.3066667".into())),
        ("database_file_guid", Val::Text("native-file-guid".into())),
        ("database_file_create_lsn", Val::Text("0".into())),
        ("database_guid", if edition == 5 { Val::Null } else { Val::Text("native-database-guid".into()) }),
        ("recovery_fork_guid", if edition == 5 { Val::Null } else { Val::Text("native-recovery-fork".into()) }),
    ]);
    replace(&Rec::of(&values), &[("session_id", Val::Null), ("edition", Val::Int(edition))])
}
fn reader(replies: Vec<crate::read::ReadResult<Vec<Rec>>>, identity: IdentityDetail) -> ActivityReader<FakeRead> {
    ActivityReader::new(
        FakeRead::new(replies),
        ReadOptions { budget: CollectionBudget::default(), scope: ActivityScope::CurrentDatabase, identity },
        "fixture.invalid".into(),
        1433,
    )
}

#[tokio::test]
async fn boxed_authority_reuses_single_native_platform_and_process_projection_without_azure_views() {
    let mut reader =
        reader(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![row(3)])], IdentityDetail::Endpoint);
    let authority = DatabaseContext::initial(&mut reader).await.unwrap();
    assert!(authority.is_qualified());
    let context = authority.capture_context().unwrap();
    assert!(context.database.incarnation.is_none(), "local native GUID is not a global database namespace");
    let other_host =
        culprit_core::DatabaseIdentity::server(culprit_core::Engine::Mssql, "shop", "another-host.invalid", 1433, None);
    assert_ne!(context.database.telemetry_id().unwrap(), other_host.telemetry_id().unwrap());
    assert!(context.epoch.is_some());
    assert_eq!(context.at, "2020-01-01T06:00:00.000000000Z");
    let identity = authority.native_identity().unwrap();
    assert_eq!(identity.qualifiers["recovery_fork_guid"], "native-recovery-fork");
    assert_eq!(reader.query_count(), 3);
    assert!(reader.io.calls.iter().all(|call| !call.sql.contains("sys.dm_user_db_resource_governance")
        && !call.sql.contains("sys.dm_exec_requests")
        && !call.sql.contains("sys.dm_db_page_info")));
    assert!(reader.io.calls[2].sql.contains("LEFT JOIN sys.database_recovery_status"));
}

#[tokio::test]
async fn azure_platform_never_reads_instance_ticks_or_the_unsupported_recovery_catalog() {
    let platform =
        replace(&metadata(), &[("edition", Val::Int(5)), ("machine_name", Val::Null), ("process_id", Val::Null)]);
    let mut reader = reader(vec![Ok(vec![platform]), Ok(vec![row(5)]), Ok(vec![row(5)])], IdentityDetail::Endpoint);
    let initial = DatabaseContext::initial(&mut reader).await.unwrap();
    assert!(initial.is_qualified());
    assert!(initial.instance.is_none());
    let final_context = DatabaseContext::recheck(&mut reader, &initial).await.unwrap();
    assert!(initial.same_source(&final_context));
    assert_eq!(reader.query_count(), 3, "two initial phases and one known-Azure recheck");
    assert!(
        reader
            .io
            .calls
            .iter()
            .all(|call| !call.sql.contains("sys.database_recovery_status") && !call.sql.contains("sys.dm_os_sys_info"))
    );
    assert!(reader.io.calls[1].sql.contains("sys.dm_user_db_resource_governance"));
}

#[tokio::test]
async fn absent_native_recovery_or_file_facts_do_not_fabricate_an_epoch() {
    for field in ["database_guid", "recovery_fork_guid", "database_file_guid"] {
        let mut reader = reader(
            vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![replace(&row(3), &[(field, Val::Null)])])],
            IdentityDetail::Endpoint,
        );
        let context = DatabaseContext::initial(&mut reader).await.unwrap();
        assert!(!context.is_qualified(), "{field}");
        assert!(context.capture_context().unwrap().epoch.is_none());
        assert!(!context.same_source(&context), "None/None is never a qualified source epoch");
        assert_eq!(reader.query_count(), 3);
    }
}

#[tokio::test]
async fn recovery_or_process_changes_invalidate_the_same_file_and_database_number() {
    for change in ["fork", "database", "process"] {
        let next = match change {
            "fork" => replace(&row(3), &[("recovery_fork_guid", Val::Text("another fork".into()))]),
            "database" => replace(&row(3), &[("database_guid", Val::Text("another database".into()))]),
            _ => row(3),
        };
        let mut reader = reader(
            vec![
                Ok(vec![metadata()]),
                Ok(vec![epoch(100)]),
                Ok(vec![row(3)]),
                Ok(vec![metadata()]),
                Ok(vec![epoch(if change == "process" { 200 } else { 100 })]),
                Ok(vec![next]),
            ],
            IdentityDetail::Endpoint,
        );
        let first = DatabaseContext::initial(&mut reader).await.unwrap();
        let after = DatabaseContext::recheck(&mut reader, &first).await.unwrap();
        assert_eq!(first.database_id, after.database_id);
        assert_eq!(first.database_file_guid, after.database_file_guid);
        assert!(!first.same_source(&after), "{change}");
        assert_ne!(first.capture_context().unwrap().epoch, after.capture_context().unwrap().epoch);
        assert_eq!(reader.query_count(), 6);
    }
}

#[tokio::test]
async fn permission_gaps_stay_unqualified_but_terminal_native_failures_stop_before_more_sql() {
    let denied = Error::new(ErrorCode::PermissionDenied, "fixture denied");
    let mut partial =
        reader(vec![Ok(vec![metadata()]), Err(denied.into()), Ok(vec![row(3)])], IdentityDetail::Endpoint);
    let authority = DatabaseContext::initial(&mut partial).await.unwrap();
    assert!(!authority.is_qualified());
    assert_eq!(authority.qualification_error, Some(ErrorCode::PermissionDenied));
    assert!(authority.capture_context().unwrap().epoch.is_none());
    assert_eq!(partial.query_count(), 3);
    let mut terminal =
        reader(vec![Ok(vec![metadata()]), Err(ReadError::Stopped(CollectionStop::TimedOut))], IdentityDetail::Endpoint);
    assert!(DatabaseContext::initial(&mut terminal).await.is_err());
    assert_eq!(terminal.query_count(), 2);
}
