use super::*;
use crate::{
    activity::{ActivityScope, IdentityDetail, ReadOptions},
    instance::test_support::{epoch, metadata},
    read::ReadError,
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::{
    Error, ErrorCode,
    telemetry::{CollectionBudget, CollectionStop},
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
    values.extend(epoch_fields(edition == 5));
    replace(&Rec::of(&values), &[("session_id", Val::Null), ("edition", Val::Int(edition))])
}

pub(crate) fn epoch_fields(visible: bool) -> Vec<(&'static str, Val)> {
    vec![
        ("local_process_id", if visible { Val::Int(11) } else { Val::Null }),
        ("local_started_ticks", if visible { Val::Int(100) } else { Val::Null }),
        ("local_now_ticks", if visible { Val::Int(1000) } else { Val::Null }),
        ("local_started_without_offset", if visible { Val::Text("2020-01-01T04:00:00".into()) } else { Val::Null }),
        ("local_epoch_visible", Val::Bool(visible)),
    ]
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
    let authority = initial(&mut reader).await.unwrap();
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
async fn azure_context_reads_its_bounded_guarded_process_epoch_without_the_boxed_recovery_catalog() {
    let platform =
        replace(&metadata(), &[("edition", Val::Int(5)), ("machine_name", Val::Null), ("process_id", Val::Null)]);
    let mut reader = reader(vec![Ok(vec![platform]), Ok(vec![row(5)]), Ok(vec![row(5)])], IdentityDetail::Endpoint);
    let initial = initial(&mut reader).await.unwrap();
    assert!(initial.is_qualified());
    assert!(initial.instance.is_none());
    let final_context = recheck(&mut reader, &initial).await.unwrap();
    assert!(initial.same_source(&final_context));
    assert_eq!(reader.query_count(), 3, "two initial phases and one known-Azure recheck");
    assert!(reader.io.calls.iter().all(|call| !call.sql.contains("sys.database_recovery_status")));
    assert!(reader.io.calls[1].sql.contains("sys.dm_user_db_resource_governance"));
    assert!(reader.io.calls[1].sql.contains("FROM sys.dm_os_sys_info OPTION(MAXDOP 1)"));
    assert!(reader.io.calls[1].sql.contains("IS_SRVROLEMEMBER('##MS_ServerStateReader##')=1"));
    assert!(reader.io.calls[1].sql.contains("ERROR_NUMBER() NOT IN(229,297,300) THROW"));
    assert_eq!(initial.source_instance().unwrap().native.id, "91487244-730d-4b83-8e49-d482241c7288");
}

#[tokio::test]
async fn missing_or_masked_azure_member_process_and_role_facts_do_not_gain_authority_from_equal_unknowns() {
    for changes in [
        vec![("replica_id", Val::Null)],
        epoch_fields(false),
        vec![("local_process_id", Val::Null)],
        vec![("updateability", Val::Null)],
    ] {
        let unqualified = replace(&row(5), &changes);
        let mut owner =
            reader(vec![Ok(vec![unqualified.clone()]), Ok(vec![unqualified])], IdentityDetail::AzureDatabase);
        let before = initial(&mut owner).await.unwrap();
        let after = recheck(&mut owner, &before).await.unwrap();
        assert!(!before.is_qualified());
        assert!(!before.same_source(&after));
        assert!(before.capture_context().unwrap().epoch.is_none());
        assert!(!before.source_partition().unwrap().is_qualified());
        assert_eq!(owner.query_count(), 2);
    }
}

#[tokio::test]
async fn current_tick_progress_does_not_change_the_native_process_epoch_but_regressed_ticks_are_invalid() {
    let before_row = row(5);
    let after_row = replace(&before_row, &[("local_now_ticks", Val::Int(2_000))]);
    let mut owner = reader(vec![Ok(vec![before_row]), Ok(vec![after_row])], IdentityDetail::AzureDatabase);
    let before = initial(&mut owner).await.unwrap();
    let after = recheck(&mut owner, &before).await.unwrap();
    assert!(before.same_source(&after));
    assert_eq!(before.capture_context().unwrap().epoch, after.capture_context().unwrap().epoch);
    let invalid = replace(&row(5), &[("local_now_ticks", Val::Int(99))]);
    let mut owner = reader(vec![Ok(vec![invalid])], IdentityDetail::AzureDatabase);
    let error = initial(&mut owner).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
}

#[tokio::test]
async fn absent_native_recovery_or_file_facts_do_not_fabricate_an_epoch() {
    for field in ["database_guid", "recovery_fork_guid", "database_file_guid"] {
        let mut reader = reader(
            vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![replace(&row(3), &[(field, Val::Null)])])],
            IdentityDetail::Endpoint,
        );
        let context = initial(&mut reader).await.unwrap();
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
        let first = initial(&mut reader).await.unwrap();
        let after = recheck(&mut reader, &first).await.unwrap();
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
    let authority = initial(&mut partial).await.unwrap();
    assert!(!authority.is_qualified());
    assert_eq!(authority.qualification_error, Some(ErrorCode::PermissionDenied));
    assert!(authority.capture_context().unwrap().epoch.is_none());
    assert_eq!(partial.query_count(), 3);
    let mut terminal =
        reader(vec![Ok(vec![metadata()]), Err(ReadError::Stopped(CollectionStop::TimedOut))], IdentityDetail::Endpoint);
    assert!(initial(&mut terminal).await.is_err());
    assert_eq!(terminal.query_count(), 2);
}
