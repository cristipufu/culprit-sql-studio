use super::*;
use crate::{
    activity::page_targets::tests::{boxed_context, header, page_wait, participant},
    activity::{ActivityReader, ActivityScope, IdentityDetail, ReadOptions, tests::details},
    instance::test_support::{epoch, metadata},
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

const AT: &str = "2020-01-01T06:00:00Z";

async fn resource() -> SharedResourceIdentity {
    crate::instance::read(&mut FakeRead::new(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)])]))
        .await
        .unwrap()
        .0
        .identity
}

fn native_reader(replies: Vec<ReadResult<Vec<Rec>>>) -> ActivityReader<FakeRead> {
    ActivityReader::new(
        FakeRead::new(replies),
        ReadOptions {
            budget: CollectionBudget::default(),
            scope: ActivityScope::CurrentDatabase,
            identity: IdentityDetail::Endpoint,
        },
        "fixture.invalid".into(),
        1433,
    )
}

fn tempdb(file: Option<u32>) -> Rec {
    Rec::of(&[
        ("observed_at", Val::Text(AT.into())),
        ("tempdb_database_id", Val::Int(2)),
        ("tempdb_name", Val::Text("tempdb".into())),
        ("tempdb_created_without_offset", Val::Text("2020-01-01T04:00:00".into())),
        ("tempdb_database_guid", Val::Text("native-tempdb-guid".into())),
        ("tempdb_recovery_fork_guid", Val::Text("native-tempdb-recovery-fork".into())),
        ("tempdb_page_permission", Val::Int(1)),
        ("selected_file_id", file.map_or(Val::Null, |file| Val::Int(i64::from(file)))),
        ("file_guid", file.map_or(Val::Null, |_| Val::Text("native-tempdb-file".into()))),
        ("file_create_lsn", file.map_or(Val::Null, |_| Val::Text("0".into()))),
        ("file_type", file.map_or(Val::Null, |_| Val::Int(0))),
        ("file_state", file.map_or(Val::Null, |_| Val::Int(0))),
    ])
}

fn wait() -> Rec {
    replace(&page_wait(42, "PAGELATCH_EX", "2:1:4144"), &[("edition", Val::Int(3))])
}

fn tempdb_header(ordinal: usize) -> Rec {
    replace(
        &header(ordinal, 4144),
        &[
            ("target_database_id", Val::Int(2)),
            ("database_id", Val::Int(2)),
            ("file_guid", Val::Text("native-tempdb-file".into())),
            ("page_type", Val::Int(11)),
            ("object_id", Val::Null),
            ("index_id", Val::Null),
            ("partition_id", Val::Null),
            ("alloc_unit_id", Val::Null),
        ],
    )
}

fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    let wait = wait();
    vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![boxed_context()]),
        Ok(vec![tempdb(None)]),
        Ok(vec![wait.clone()]),
        Ok(vec![participant(0, &wait)]),
        Ok(vec![tempdb_header(0)]),
        Ok(vec![details(42, "INSERT dbo.work VALUES(1)")]),
        Ok(vec![tempdb_header(0)]),
        Ok(vec![participant(0, &wait)]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![boxed_context()]),
        Ok(vec![tempdb(None)]),
    ]
}

#[tokio::test]
async fn shared_tempdb_headers_keep_the_original_application_request_and_verify_the_selected_file_before_publication() {
    let resource = resource().await;
    let mut scripted = replies();
    scripted.extend([Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![boxed_context()]), Ok(vec![tempdb(Some(1))])]);
    let mut reader = native_reader(scripted);
    let captured = reader.capture_shared_tempdb_page_wait_targets(&resource).await.unwrap();
    assert_eq!(reader.query_count(), 14);
    assert_eq!(captured.collection_target(), CollectionTarget::Shared(resource.clone()));
    assert!(
        matches!(captured.source_partition().unwrap(),SourcePartition::InstanceLocal{member:Some(member),epoch:Some(_)} if member==resource)
    );
    let row = &captured.snapshot.observations[0];
    let target = row.target.as_ref().unwrap();
    assert_eq!(row.request.key.session.id, "42");
    assert_eq!(row.request.database.as_deref(), Some("shop"));
    assert_eq!(target.header.database_id, 2);
    assert_eq!(target.kind, PageWaitTargetKind::Pfs);
    assert!(target.object.is_none());
    assert!(matches!(target.scope, PageTargetScope::SharedResource { .. }));
    assert!(matches!(row.index_operations, Some(PageIndexOperations::Unavailable)));
    assert!(reader.io.calls.iter().filter(|call| call.sql.contains("OUTER APPLY sys.dm_db_page_info(2,")).count() == 2);
    assert!(!reader.io.calls.iter().any(|call| call.sql.contains("OUTER APPLY sys.dm_db_page_info(DB_ID()")));
    assert!(!reader.io.calls.iter().any(|call| call.sql.contains("sys.dm_db_index_operational_stats")));
    assert!(reader.verify_page_wait_context(&captured.guard).await.unwrap());
    assert_eq!(reader.query_count(), 18);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(reader.io.closed, 1);
}

#[tokio::test]
async fn another_native_member_cannot_authorize_a_tempdb_read_from_the_configured_route() {
    let mut resource = resource().await;
    resource.native.id = "other-instance".into();
    let mut reader = native_reader(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![boxed_context()])]);
    let error = reader.capture_shared_tempdb_page_wait_targets(&resource).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert_eq!(reader.query_count(), 3);
    assert!(!reader.io.calls.iter().any(|call| call.sql.contains("tempdb.sys")));
}

#[tokio::test]
async fn tempdb_database_recreation_and_file_replacement_cannot_pass_the_existing_source_guard() {
    let resource = resource().await;
    for field in ["tempdb_created_without_offset", "tempdb_database_guid", "tempdb_recovery_fork_guid"] {
        let mut scripted = replies();
        let value =
            if field == "tempdb_created_without_offset" { "2020-01-01T04:01:00" } else { "replaced-native-identity" };
        scripted[13] = Ok(vec![replace(&tempdb(None), &[(field, Val::Text(value.into()))])]);
        let mut reader = native_reader(scripted);
        let error = reader.capture_shared_tempdb_page_wait_targets(&resource).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert_eq!(reader.query_count(), 14);
    }
    let mut scripted = replies();
    scripted.extend([
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![boxed_context()]),
        Ok(vec![replace(&tempdb(Some(1)), &[("file_guid", Val::Text("replaced-file".into()))])]),
    ]);
    let mut reader = native_reader(scripted);
    let captured = reader.capture_shared_tempdb_page_wait_targets(&resource).await.unwrap();
    assert!(!reader.verify_page_wait_context(&captured.guard).await.unwrap());
}

#[tokio::test]
async fn shared_permission_gaps_keep_observed_waits_without_running_a_header_function() {
    let resource = resource().await;
    let denied = replace(&tempdb(None), &[("tempdb_page_permission", Val::Int(0))]);
    let mut reader = native_reader(vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![boxed_context()]),
        Ok(vec![denied.clone()]),
        Ok(vec![wait()]),
        Ok(vec![details(42, "INSERT dbo.work VALUES(1)")]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![boxed_context()]),
        Ok(vec![denied]),
    ]);
    let captured = reader.capture_shared_tempdb_page_wait_targets(&resource).await.unwrap();
    assert_eq!(captured.snapshot.observations[0].resolution, PageWaitResolution::PermissionDenied);
    assert!(captured.snapshot.observations[0].target.is_none());
    assert!(reader.io.calls.iter().all(|call| !call.sql.contains("OUTER APPLY sys.dm_db_page_info")));
    assert_eq!(reader.query_count(), 10);
}

#[tokio::test]
async fn a_cancelled_shared_header_read_stops_followup_and_unconfirmed_shutdown_is_not_quiet() {
    let resource = resource().await;
    let mut scripted = replies();
    scripted[6] = Err(ReadError::Stopped(CollectionStop::Cancelled));
    let mut reader = native_reader(scripted);
    reader.io.shutdown = SourceShutdown::Unconfirmed;
    let error = reader.capture_shared_tempdb_page_wait_targets(&resource).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::Cancelled);
    assert_eq!(reader.query_count(), 7);
    assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(reader.io.closed, 1);
}

#[tokio::test]
async fn one_scan_population_can_preserve_app_pages_and_shared_tempdb_pages_without_duplicate_header_roundtrips() {
    let app = replace(&page_wait(43, "PAGELATCH_EX", "7:1:4145"), &[("edition", Val::Int(3))]);
    let mut scripted = replies();
    scripted[4] = Ok(vec![wait(), app.clone()]);
    scripted[5] = Ok(vec![participant(0, &wait()), participant(1, &app)]);
    scripted[6] = Ok(vec![tempdb_header(0), header(1, 4145)]);
    scripted[7] = Ok(vec![details(42, "INSERT dbo.work VALUES(1)"), details(43, "INSERT dbo.orders VALUES(1)")]);
    scripted[8] = Ok(vec![tempdb_header(0), header(1, 4145)]);
    scripted[9] = Ok(vec![participant(0, &wait()), participant(1, &app)]);
    let mut reader = native_reader(scripted);
    let captured = reader.capture_scan_page_wait_targets(true).await.unwrap();
    assert!(matches!(captured.collection_target(), CollectionTarget::Database(_)));
    assert_eq!(captured.snapshot.observations.len(), 2);
    assert!(matches!(
        captured.snapshot.observations[0].target.as_ref().unwrap().scope,
        PageTargetScope::SharedResource { .. }
    ));
    assert!(matches!(
        captured.snapshot.observations[1].target.as_ref().unwrap().scope,
        PageTargetScope::RequestDatabase
    ));
    assert_eq!(reader.io.calls.iter().filter(|call| call.sql.contains("FROM sys.dm_exec_requests r")).count(), 1);
    let headers =
        reader.io.calls.iter().filter(|call| call.sql.contains("OUTER APPLY sys.dm_db_page_info")).collect::<Vec<_>>();
    assert_eq!(headers.len(), 2);
    for call in headers {
        assert!(call.sql.contains("sys.dm_db_page_info(DB_ID()"));
        assert!(call.sql.contains("sys.dm_db_page_info(2,"));
        assert!(call.sql.contains("LEFT JOIN tempdb.sys.all_objects"));
        assert!(call.sql.contains("@targets.nodes('/pages/request_database/page')"));
        assert!(call.sql.contains("@targets.nodes('/pages/shared_tempdb/page')"));
        let [crate::read::Parameter::Text(xml)] = call.parameters.as_slice() else { panic!() };
        assert_eq!(
            xml,
            "<pages><request_database><page ordinal=\"1\" database=\"7\" file=\"1\" page=\"4145\"/></request_database><shared_tempdb><page ordinal=\"0\" database=\"2\" file=\"1\" page=\"4144\"/></shared_tempdb></pages>"
        );
        assert!(call.limit <= 8);
    }
    assert_eq!(reader.query_count(), 14);
}

#[tokio::test]
async fn an_explicit_shared_focus_keeps_its_original_request_and_never_discovers_another_waiter() {
    let original = crate::activity::page_targets::tests::page_wait(42, "PAGELATCH_EX", "2:1:4144");
    let mut focus = crate::activity::page_targets::focus::tests::focus(&original);
    let mut authority = native_reader(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![boxed_context()])]);
    let native = crate::activity::database_context::initial(&mut authority).await.unwrap();
    let original_context = native.capture_context().unwrap();
    focus.target = original_context.database.into();
    focus.epoch = original_context.epoch.unwrap();
    focus.partition = native.source_partition().unwrap();
    let resource = resource().await;
    let partition = SourcePartition::InstanceLocal {
        member: Some(resource.clone()),
        epoch: Some(native.instance.as_ref().unwrap().capture_epoch().unwrap()),
    };
    let mut reader = native_reader(replies());
    let captured = reader.capture_shared_tempdb_page_wait_focus(&focus, &resource, &partition).await.unwrap();
    assert_eq!(captured.result.focus, focus);
    assert!(captured.result.permits_page_lookup());
    assert_eq!(captured.capture.snapshot.observations.len(), 1);
    assert!(matches!(
        captured.capture.snapshot.observations[0].target.as_ref().unwrap().scope,
        PageTargetScope::SharedResource { .. }
    ));
    assert!(reader.io.calls[4].sql.contains("r.session_id=@P2 AND r.request_id=@P3"));
    assert!(!reader.io.calls[4].sql.contains("sys.dm_os_waiting_tasks"));
    assert_eq!(reader.query_count(), 14);
}

#[tokio::test]
async fn stale_shared_execution_generation_refuses_before_tempdb_metadata_activity_or_headers() {
    let original = crate::activity::page_targets::tests::page_wait(42, "PAGELATCH_EX", "2:1:4144");
    let mut focus = crate::activity::page_targets::focus::tests::focus(&original);
    let mut authority = native_reader(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![boxed_context()])]);
    let native = crate::activity::database_context::initial(&mut authority).await.unwrap();
    let current = native.capture_context().unwrap();
    focus.target = current.database.into();
    focus.epoch = current.epoch.unwrap();
    focus.partition = native.source_partition().unwrap();
    let resource = resource().await;
    for stale_member in [false, true] {
        let mut member = resource.clone();
        if stale_member {
            member.native.id = "other-native-instance".into();
        }
        let expected = SourcePartition::InstanceLocal {
            member: Some(member),
            epoch: Some("stale-measured-native-generation".into()),
        };
        let mut reader = native_reader(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![boxed_context()])]);
        let error = reader.capture_shared_tempdb_page_wait_focus(&focus, &resource, &expected).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert_eq!(reader.query_count(), 3);
        assert!(!reader.io.calls.iter().any(|call| call.sql.contains("tempdb.")
            || call.sql.contains("FROM sys.dm_exec_requests r")
            || call.sql.contains("sys.dm_db_page_info(")));
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn a_shared_subject_is_not_permission_to_inspect_other_foreign_databases() {
    let resource = resource().await;
    let foreign = replace(&page_wait(42, "PAGELATCH_EX", "3:1:4144"), &[("edition", Val::Int(3))]);
    let mut reader = native_reader(vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![boxed_context()]),
        Ok(vec![tempdb(None)]),
        Ok(vec![foreign]),
        Ok(vec![details(42, "INSERT dbo.work VALUES(1)")]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![boxed_context()]),
        Ok(vec![tempdb(None)]),
    ]);
    let captured = reader.capture_shared_tempdb_page_wait_targets(&resource).await.unwrap();
    assert_eq!(captured.snapshot.observations[0].resolution, PageWaitResolution::OutsideDatabase);
    assert!(captured.snapshot.observations[0].target.is_none());
    assert!(reader.io.calls.iter().all(|call| !call.sql.contains("OUTER APPLY sys.dm_db_page_info")));
}

#[tokio::test]
async fn native_process_changes_and_total_query_exhaustion_never_publish_a_shared_target() {
    let resource = resource().await;
    let mut scripted = replies();
    scripted[11] = Ok(vec![epoch(101)]);
    let mut reader = native_reader(scripted);
    let error = reader.capture_shared_tempdb_page_wait_targets(&resource).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert_eq!(reader.query_count(), 13);

    let mut reader = native_reader(replies());
    reader.io = reader.io.with_query_limit(13);
    let error = reader.capture_shared_tempdb_page_wait_targets(&resource).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::QueryLimit);
    assert_eq!(reader.query_count(), 13);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn unavailable_shared_proof_does_not_remove_readable_application_pages_from_a_combined_scan() {
    let app = replace(&page_wait(43, "PAGELATCH_EX", "7:1:4145"), &[("edition", Val::Int(3))]);
    let denied = replace(&tempdb(None), &[("tempdb_page_permission", Val::Int(0))]);
    let mut scripted = replies();
    scripted[3] = Ok(vec![denied.clone()]);
    scripted[4] = Ok(vec![wait(), app.clone()]);
    scripted[5] = Ok(vec![participant(1, &app)]);
    scripted[6] = Ok(vec![header(0, 4145)]);
    scripted[7] = Ok(vec![details(42, "INSERT dbo.work VALUES(1)"), details(43, "INSERT dbo.orders VALUES(1)")]);
    scripted[8] = Ok(vec![header(0, 4145)]);
    scripted[9] = Ok(vec![participant(1, &app)]);
    scripted[13] = Ok(vec![denied]);
    let mut reader = native_reader(scripted);
    let captured = reader.capture_scan_page_wait_targets(true).await.unwrap();
    assert_eq!(captured.snapshot.observations.len(), 2);
    assert_eq!(captured.snapshot.observations[0].resolution, PageWaitResolution::PermissionDenied);
    assert!(captured.snapshot.observations[0].target.is_none());
    assert!(matches!(
        captured.snapshot.observations[1].target.as_ref().unwrap().scope,
        PageTargetScope::RequestDatabase
    ));
    assert!(!reader.io.calls.iter().any(|call| call.sql.contains("OUTER APPLY sys.dm_db_page_info(2,")));
}

#[tokio::test]
async fn an_azure_database_route_does_not_invent_a_tempdb_instance_from_a_replica_or_database_id() {
    let resource = resource().await;
    let mut reader =
        crate::activity::page_targets::tests::reader(vec![Ok(vec![crate::activity::page_targets::tests::context()])]);
    let error = reader.capture_shared_tempdb_page_wait_targets(&resource).await.err().unwrap();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert_eq!(reader.query_count(), 1);
    assert!(
        !reader
            .io
            .calls
            .iter()
            .any(|call| call.sql.contains("tempdb.sys") || call.sql.contains("OUTER APPLY sys.dm_db_page_info"))
    );
}

#[tokio::test]
async fn managed_instance_product_version_twelve_uses_observed_function_support_for_shared_and_application_pages() {
    let platform = replace(&metadata(), &[("edition", Val::Int(8)), ("major", Val::Text("12".into()))]);
    let context = replace(&boxed_context(), &[("edition", Val::Int(8)), ("major", Val::Int(12))]);
    let waiting = replace(&wait(), &[("edition", Val::Int(8)), ("major", Val::Int(12))]);
    let mut supplied = replies();
    for index in [0, 10] {
        supplied[index] = Ok(vec![platform.clone()]);
    }
    for index in [2, 12] {
        supplied[index] = Ok(vec![context.clone()]);
    }
    supplied[4] = Ok(vec![waiting.clone()]);
    for index in [5, 9] {
        supplied[index] = Ok(vec![participant(0, &waiting)]);
    }
    let resource = resource().await;
    let mut native = native_reader(supplied);
    let captured = native.capture_shared_tempdb_page_wait_targets(&resource).await.unwrap();
    assert_eq!(captured.snapshot.observations[0].target.as_ref().unwrap().kind, PageWaitTargetKind::Pfs);
    assert!(matches!(captured.collection_target(), CollectionTarget::Shared(_)));
    assert_eq!(native.query_count(), 14);

    let waiting =
        replace(&page_wait(42, "PAGELATCH_EX", "7:1:4144"), &[("edition", Val::Int(8)), ("major", Val::Int(12))]);
    let mut native = native_reader(vec![
        Ok(vec![platform.clone()]),
        Ok(vec![epoch(100)]),
        Ok(vec![context.clone()]),
        Ok(vec![waiting.clone()]),
        Ok(vec![participant(0, &waiting)]),
        Ok(vec![header(0, 4144)]),
        Ok(vec![details(42, "INSERT dbo.orders VALUES(1)")]),
        Ok(vec![header(0, 4144)]),
        Ok(vec![participant(0, &waiting)]),
        Ok(vec![platform]),
        Ok(vec![epoch(100)]),
        Ok(vec![context]),
    ]);
    let captured = native.capture_page_wait_targets().await.unwrap();
    assert!(matches!(
        captured.snapshot.observations[0].target.as_ref().unwrap().scope,
        PageTargetScope::RequestDatabase
    ));
    assert_eq!(native.query_count(), 12);
}

#[tokio::test]
async fn unsupported_boxed_versions_or_missing_native_functions_never_start_shared_tempdb_work() {
    let resource = resource().await;
    for (edition, major, available) in [(3, 14, true), (8, 12, false), (3, 16, false)] {
        let platform = replace(&metadata(), &[("edition", Val::Int(edition)), ("major", Val::Text(major.to_string()))]);
        let context = replace(
            &boxed_context(),
            &[
                ("edition", Val::Int(edition)),
                ("major", Val::Int(major)),
                ("page_info_available", Val::Int(i64::from(available))),
            ],
        );
        let mut native = native_reader(vec![Ok(vec![platform]), Ok(vec![epoch(100)]), Ok(vec![context])]);
        let error = native.capture_shared_tempdb_page_wait_targets(&resource).await.err().unwrap();
        assert_eq!(error.code, ErrorCode::Unsupported);
        assert_eq!(native.query_count(), 3);
        assert!(native.io.calls.iter().all(|call|!call.sql.contains("FROM tempdb.")&&!call.sql.contains("OUTER APPLY sys.dm_db_page_info")));
    }
}
