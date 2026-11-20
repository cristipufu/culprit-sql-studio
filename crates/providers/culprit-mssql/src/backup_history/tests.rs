use super::*;
use crate::{
    instance::test_support::{epoch, metadata as platform_row},
    read::ReadResult,
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};
pub(crate) const DATABASE: &str = "10000000-0000-0000-0000-000000000001";
pub(crate) const FORK: &str = "30000000-0000-0000-0000-000000000003";
pub(crate) const PHYSICAL: &str = "40000000-0000-0000-0000-000000000004";
pub(crate) fn platform(edition: i64, at: &str) -> Rec {
    replace(&platform_row(), &[("edition", Val::Int(edition)), ("observed_at", Val::Text(at.into()))])
}
pub(crate) fn local(edition: i64, at: &str) -> Rec {
    replace(
        &crate::activity::database_context::tests::row(edition),
        &[
            ("database_guid", Val::Text(DATABASE.into())),
            ("recovery_fork_guid", Val::Text(FORK.into())),
            ("observed_at", Val::Text(at.into())),
        ],
    )
}
pub(crate) fn azure_authority() -> Rec {
    Rec::of(&[
        ("database_name", Val::Text("shop".into())),
        ("incarnation", Val::Text(DATABASE.into())),
        ("physical_incarnation", Val::Text(PHYSICAL.into())),
        ("updateability", Val::Text("READ_WRITE".into())),
    ])
}
pub(crate) fn metadata_row(edition: i64, at: &str) -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(7)),
        ("database_name", Val::Text("shop".into())),
        ("engine_edition", Val::Int(edition)),
        ("major", Val::Text("16".into())),
        ("database_edition", Val::Text("General Purpose".into())),
        ("replica_id", Val::Null),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("observed_at", Val::Text(at.into())),
        ("layout_available", Val::Int(1)),
    ])
}
pub(crate) fn msdb_row() -> Rec {
    Rec::of(&[
        ("selected_database_id", Val::Int(7)),
        ("selected_database_name", Val::Text("shop".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("observed_at", Val::Text("2020-01-01T06:00:00.5Z".into())),
        ("selected_database_guid", Val::Text(DATABASE.to_uppercase())),
        ("selected_recovery_fork_guid", Val::Text(FORK.to_uppercase())),
        ("backup_set_id", Val::Int(1)),
        ("backup_set_uuid", Val::Text("20000000-0000-0000-0000-000000000002".into())),
        ("database_guid", Val::Text(DATABASE.into())),
        ("database_name", Val::Text("shop".into())),
        ("family_guid", Val::Null),
        ("first_recovery_fork_guid", Val::Null),
        ("last_recovery_fork_guid", Val::Null),
        ("started_without_offset", Val::Text("2019-12-31T21:00:00.123".into())),
        ("finished_without_offset", Val::Null),
        ("time_zone", Val::Int(127)),
        ("backup_type", Val::Text("D".into())),
        ("size_bytes", Val::Text("18446744073709551616".into())),
        ("compressed_size_bytes", Val::Null),
        ("user_name", Val::Text("historical-user".into())),
        ("server_name", Val::Null),
        ("machine_name", Val::Null),
        ("software_vendor_id", Val::Int(4608)),
        ("software_major_version", Val::Int(16)),
        ("software_minor_version", Val::Int(0)),
        ("software_build_version", Val::Int(4325)),
        ("first_lsn", Val::Text("9999999999999999999999999".into())),
        ("last_lsn", Val::Null),
        ("checkpoint_lsn", Val::Null),
        ("database_backup_lsn", Val::Null),
        ("differential_base_lsn", Val::Null),
        ("fork_point_lsn", Val::Null),
        ("is_copy_only", Val::Int(1)),
        ("is_snapshot", Val::Null),
        ("has_backup_checksums", Val::Int(0)),
        ("is_damaged", Val::Null),
        ("begins_log_chain", Val::Null),
        ("has_incomplete_metadata", Val::Null),
    ])
}
pub(crate) fn azure_row() -> Rec {
    Rec::of(&[
        ("selected_database_id", Val::Int(7)),
        ("selected_database_name", Val::Text("shop".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("observed_at", Val::Text("2020-01-01T06:00:00.5Z".into())),
        ("native_identity_rows", Val::Int(1)),
        ("incarnation", Val::Text(DATABASE.into())),
        ("physical_incarnation", Val::Text(PHYSICAL.into())),
        ("engine_edition", Val::Int(5)),
        ("database_name", Val::Text("shop".into())),
        ("backup_file_id", Val::Text("20000000-0000-0000-0000-000000000002".into())),
        ("logical_database_id", Val::Text(DATABASE.into())),
        ("physical_database_name", Val::Text("native-physical".into())),
        ("logical_server_name", Val::Text("native-server".into())),
        ("logical_database_name", Val::Text("historical-db-name".into())),
        ("started_without_offset", Val::Text("2019-12-31T21:00:00.1234567".into())),
        ("finished_without_offset", Val::Text("2019-12-31T21:01:00.7654321".into())),
        ("backup_type", Val::Text("I".into())),
        ("in_retention", Val::Int(0)),
    ])
}
#[derive(Default)]
pub(crate) struct Trace {
    pub sql: Mutex<Vec<String>>,
    pub closed: AtomicU32,
}
pub(crate) struct Reader {
    pub inner: FakeRead,
    pub trace: Arc<Trace>,
    pub generation: Option<u64>,
    pub change_at: Option<u32>,
}
#[async_trait::async_trait]
impl NativeRead for Reader {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.trace.sql.lock().unwrap().push(sql.into());
        self.inner.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.generation.map(|value| value + u64::from(self.change_at.is_some_and(|at| self.queries() >= at)))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.trace.closed.fetch_add(1, Ordering::SeqCst);
        self.inner.close().await
    }
}
pub(crate) fn reader(replies: Vec<ReadResult<Vec<Rec>>>) -> Reader {
    Reader { inner: FakeRead::new(replies), trace: Arc::new(Trace::default()), generation: Some(1), change_at: None }
}
pub(crate) fn authority_replies(edition: i64, at: &str) -> Vec<ReadResult<Vec<Rec>>> {
    if edition == 5 {
        vec![Ok(vec![platform(5, at)]), Ok(vec![azure_authority()])]
    } else {
        vec![
            Ok(vec![platform(edition, at)]),
            Ok(vec![platform(edition, at)]),
            Ok(vec![epoch(100)]),
            Ok(vec![local(edition, at)]),
        ]
    }
}
async fn held(edition: i64) -> Authority {
    Authority::read(&mut reader(authority_replies(edition, "2020-01-01T06:00:00Z")), "fixture.invalid", 1433)
        .await
        .unwrap()
}
#[tokio::test]
async fn one_native_decoder_preserves_all_azure_fields_and_exact_msdb_numeric_units() {
    for edition in [3, 5, 8] {
        let authority = held(edition).await;
        let mut io = reader(vec![
            Ok(vec![metadata_row(edition, "2020-01-01T06:00:00Z")]),
            Ok(vec![if edition == 5 { azure_row() } else { msdb_row() }]),
        ]);
        let metadata = metadata(&mut io, &authority).await.unwrap();
        let points = read(&mut io, &authority, &metadata, 64).await.unwrap();
        assert_eq!(io.inner.calls[1].limit, 65);
        assert!(matches!(io.inner.calls[1].parameters[0], Parameter::Integer(65)));
        let context = NativeReadContext {
            target: authority.database().into(),
            partition: authority.partition().unwrap(),
            observed_from: authority.at().into(),
            observed_until: "2020-01-01T06:00:01Z".into(),
            database_writable: Some(true),
        };
        points[0].validate(&context, &metadata.scope).unwrap();
        match &points[0].record {
            MssqlBackupRecord::AzureSqlDatabase(row) => {
                assert_eq!(row.logical_database_id, DATABASE);
                assert_eq!(row.logical_database_name, "historical-db-name");
                assert_eq!(row.physical_database_name, "native-physical");
                assert_eq!(row.logical_server_name, "native-server");
                assert_eq!(row.started_without_offset, "2019-12-31T21:00:00.1234567");
                assert_eq!(row.finished_without_offset, "2019-12-31T21:01:00.7654321");
                assert_eq!(row.backup_type, MssqlBackupType::DifferentialDatabase);
                assert!(!row.in_retention);
            }
            MssqlBackupRecord::Msdb(row) => {
                assert_eq!(row.size_bytes.as_deref(), Some("18446744073709551616"));
                assert_eq!(row.lsns.first.as_deref(), Some("9999999999999999999999999"));
                assert_eq!(row.started_without_offset.as_deref(), Some("2019-12-31T21:00:00.1230000"));
                assert_eq!(row.finished_without_offset, None);
                assert_eq!(row.time_zone_quarter_hours, Some(127));
                assert_eq!(row.initiator_user_name.as_deref(), Some("historical-user"));
                assert_eq!(row.flags.checksums, Some(false));
            }
        }
    }
}
#[tokio::test]
async fn ambiguous_foreign_or_unknown_frame_authority_is_not_repaired_by_final_metadata() {
    for (edition, field, value) in [
        (3, "selected_database_id", Val::Int(8)),
        (3, "selected_database_guid", Val::Text(PHYSICAL.into())),
        (3, "selected_recovery_fork_guid", Val::Text(PHYSICAL.into())),
        (3, "updateability", Val::Null),
        (5, "native_identity_rows", Val::Int(2)),
        (5, "incarnation", Val::Text(PHYSICAL.into())),
        (5, "physical_incarnation", Val::Text(DATABASE.into())),
    ] {
        let authority = held(edition).await;
        let row = if edition == 5 { azure_row() } else { msdb_row() };
        let mut io = reader(vec![
            Ok(vec![metadata_row(edition, "2020-01-01T06:00:00Z")]),
            Ok(vec![replace(&row, &[(field, value)])]),
        ]);
        let metadata = metadata(&mut io, &authority).await.unwrap();
        assert!(read(&mut io, &authority, &metadata, 64).await.is_err(), "{edition}/{field}");
        assert_eq!(io.queries(), 2);
    }
}
#[tokio::test]
async fn duplicate_native_keys_and_malformed_nullable_payloads_are_not_healthy_empty() {
    let authority = held(3).await;
    for (field, value) in [
        ("backup_set_uuid", Val::Text("not-a-guid".into())),
        ("time_zone", Val::Float(0.25)),
        ("first_lsn", Val::Float(9.0)),
        ("is_copy_only", Val::Int(2)),
        ("started_without_offset", Val::Text("2020-01-01T00:00:00Z".into())),
    ] {
        let mut io = reader(vec![
            Ok(vec![metadata_row(3, "2020-01-01T06:00:00Z")]),
            Ok(vec![replace(&msdb_row(), &[(field, value)])]),
        ]);
        let metadata = metadata(&mut io, &authority).await.unwrap();
        assert!(read(&mut io, &authority, &metadata, 64).await.is_err(), "{field}");
    }
    let mut io = reader(vec![Ok(vec![metadata_row(3, "2020-01-01T06:00:00Z")]), Ok(vec![msdb_row(), msdb_row()])]);
    let metadata = metadata(&mut io, &authority).await.unwrap();
    assert!(read(&mut io, &authority, &metadata, 64).await.is_err());
}
#[tokio::test]
async fn actual_layout_and_deployment_facts_control_availability_without_reading_history() {
    let azure = held(5).await;
    for tier in [Some("Hyperscale"), Some("future-tier"), None] {
        let row = replace(
            &metadata_row(5, "2020-01-01T06:00:00Z"),
            &[("database_edition", tier.map(|tier| Val::Text(tier.into())).unwrap_or(Val::Null))],
        );
        let mut io = reader(vec![Ok(vec![row])]);
        let metadata = metadata(&mut io, &azure).await.unwrap();
        assert!(metadata.unavailable().is_some());
        assert_eq!(metadata.scope.hyperscale, if tier == Some("Hyperscale") { Some(true) } else { None });
        assert!(read(&mut io, &azure, &metadata, 64).await.is_err());
        assert_eq!(io.queries(), 1);
    }
    let local = held(3).await;
    let mut io =
        reader(vec![Ok(vec![replace(&metadata_row(3, "2020-01-01T06:00:00Z"), &[("layout_available", Val::Int(0))])])]);
    assert!(metadata(&mut io, &local).await.unwrap().unavailable().is_some());
    assert_eq!(io.queries(), 1);
}
#[test]
fn history_sql_is_selected_native_catalog_only_and_keeps_one_bounded_result_set() {
    for profile in [
        MssqlBackupHistoryProfile::AzureSqlDatabase,
        MssqlBackupHistoryProfile::SqlServerMsdb,
        MssqlBackupHistoryProfile::ManagedInstanceMsdb,
    ] {
        let sql = sql::history(profile);
        assert!(sql.contains("SELECT TOP(@P1) /* culprit */"));
        assert!(sql.ends_with("OPTION(MAXDOP 1)"));
        let layout = sql::metadata(profile);
        assert!(layout.contains("c.precision<>expected.native_precision"));
        if profile != MssqlBackupHistoryProfile::AzureSqlDatabase {
            assert!(layout.contains("(N'backup_size',108,0,20)"));
            assert!(layout.contains("(N'first_lsn',108,0,25)"));
            assert!(sql.contains("CONVERT(varchar(40),b.first_lsn)"));
        }
        assert_eq!(sql.matches("AS selected_database_id").count(), 1);
        for forbidden in [
            "backupmedia",
            "backupfile",
            "physical_device",
            "dm_exec_sql",
            "RESTORE ",
            "BACKUP DATABASE",
            "CREATE ",
            "DROP ",
            "COUNT(*)",
            "sys.databases",
        ] {
            assert!(!sql.contains(forbidden), "{profile:?}: {forbidden}");
        }
        if profile == MssqlBackupHistoryProfile::AzureSqlDatabase {
            assert!(sql.contains("WHERE b.logical_database_id=CONVERT(uniqueidentifier,@P2)"));
            assert!(sql.contains("SELECT TOP(2)"));
        } else {
            assert!(sql.contains("WHERE b.database_guid=CONVERT(uniqueidentifier,@P2) AND b.database_name=@P3"));
            assert!(sql.contains("ORDER BY b.backup_set_id DESC"));
            assert!(!sql.contains("JOIN"));
        }
    }
}
