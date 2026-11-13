use super::*;
const HEADER_BEFORE: usize = 4;
const HEADER_AFTER: usize = 7;
const CHECK_AFTER: usize = 8;
const SQL_TEXT: usize = 5;
const FINAL_CONTEXT: usize = 9;
use crate::{
    activity::{
        IdentityDetail, ReadOptions,
        tests::{details, metadata},
    },
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

pub(crate) fn reader(replies: Vec<ReadResult<Vec<Rec>>>) -> ActivityReader<FakeRead> {
    reader_with_identity(replies, IdentityDetail::AzureDatabase)
}
fn reader_with_identity(replies: Vec<ReadResult<Vec<Rec>>>, identity: IdentityDetail) -> ActivityReader<FakeRead> {
    ActivityReader::new(
        FakeRead::new(replies),
        ReadOptions { budget: CollectionBudget::default(), scope: ActivityScope::CurrentDatabase, identity },
        "fixture.invalid".into(),
        1433,
    )
}

#[tokio::test]
async fn boxed_native_instance_and_file_epoch_qualify_local_mapping_without_inventing_a_database_guid() {
    let selected = replace(&context(), &[("edition", Val::Int(3)), ("database_file_create_lsn", Val::Null)]);
    let wait = replace(&page_wait(42, "PAGELATCH_EX", "7:1:4144"), &[("edition", Val::Int(3))]);
    let participant = participant(0, &wait);
    let native_instance = crate::instance::test_support::metadata();
    let native_epoch = crate::instance::test_support::epoch(100);
    let native_header = replace(&header(0, 4144), &[("file_create_lsn", Val::Null)]);
    let replies = vec![
        Ok(vec![selected.clone()]),
        Ok(vec![native_instance.clone()]),
        Ok(vec![native_epoch.clone()]),
        Ok(vec![wait]),
        Ok(vec![participant.clone()]),
        Ok(vec![native_header.clone()]),
        Ok(vec![details(42, "SELECT 1")]),
        Ok(vec![native_header]),
        Ok(vec![participant]),
        Ok(vec![selected]),
        Ok(vec![native_instance]),
        Ok(vec![native_epoch]),
    ];
    let mut reader = reader_with_identity(replies, IdentityDetail::Endpoint);
    let capture = reader.capture_page_wait_targets().await.unwrap();
    let target = capture.snapshot.observations[0].target.as_ref().unwrap();
    assert_eq!(target.identity_basis, PageTargetIdentityBasis::LocalDatabaseFileEpoch);
    assert_eq!(target.database.id, "7");
    assert!(target.database.qualifiers.contains_key("instance_epoch"));
    assert_eq!(target.file.id, "native-file-guid");
    assert!(!target.file.qualifiers.contains_key("create_lsn"));
    assert!(!target.database.qualifiers.contains_key("database_file_create_lsn"));
    assert!(capture.context.epoch.is_some());
    assert!(capture.context.database.incarnation.is_none());
    assert_eq!(reader.query_count(), 12);
}

#[tokio::test]
async fn denied_boxed_authority_preserves_waits_but_never_reads_headers_or_verifies_none_equals_none() {
    let selected = replace(&context(), &[("edition", Val::Int(3))]);
    let wait = replace(&page_wait(42, "PAGELATCH_EX", "7:1:4144"), &[("edition", Val::Int(3))]);
    let denied = || Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "fixture")));
    let mut reader = reader_with_identity(
        vec![
            Ok(vec![selected.clone()]),
            denied(),
            Ok(vec![wait]),
            Ok(vec![details(42, "SELECT 1")]),
            Ok(vec![selected.clone()]),
            denied(),
            Ok(vec![selected]),
            denied(),
        ],
        IdentityDetail::Endpoint,
    );
    let capture = reader.capture_page_wait_targets().await.unwrap();
    assert_eq!(capture.snapshot.observations[0].resolution, PageWaitResolution::PermissionDenied);
    assert!(capture.snapshot.observations[0].target.is_none());
    assert!(!reader.verify_page_wait_context(&capture.guard).await.unwrap());
    assert!(reader.io.calls.iter().all(|call| !call.sql.contains("OUTER APPLY sys.dm_db_page_info")));
}

#[tokio::test]
async fn a_request_that_advances_before_header_admission_never_triggers_a_page_lookup() {
    let row = page_wait(42, "PAGELATCH_EX", "7:1:4144");
    let checked = replace(&participant(0, &row), &[("checked_begin", Val::Int(2))]);
    let mut reader = reader(vec![
        Ok(vec![context()]),
        Ok(vec![row]),
        Ok(vec![checked]),
        Ok(vec![details(42, "SELECT 1")]),
        Ok(vec![context()]),
    ]);
    let capture = reader.capture_page_wait_targets().await.unwrap();
    assert_eq!(capture.snapshot.observations[0].resolution, PageWaitResolution::ChangedRequest);
    assert!(reader.io.calls.iter().all(|call| !call.sql.contains("OUTER APPLY sys.dm_db_page_info")));
}
pub(crate) fn context() -> Rec {
    let base = metadata(42);
    let mut values = base.columns().map(|(key, value)| (key, value.clone())).collect::<Vec<_>>();
    values.extend([
        ("selected_database_id", Val::Int(7)),
        ("database_created_without_offset", Val::Text("2019-01-01T04:00:00.3066667".into())),
        ("database_file_guid", Val::Text("native-file-guid".into())),
        ("database_file_create_lsn", Val::Text("0".into())),
        ("page_info_available", Val::Int(1)),
        ("page_permission", Val::Int(1)),
    ]);
    replace(&Rec::of(&values), &[("session_id", Val::Null)])
}
pub(crate) fn page_wait(session: i64, wait_type: &str, address: &str) -> Rec {
    replace(
        &metadata(session),
        &[
            ("wait_type", Val::Text(wait_type.into())),
            ("wait_resource", Val::Text(address.into())),
            ("elapsed_ms", Val::Int(1)),
            ("wait_ms", Val::Int(1)),
            ("blocking_session_id", Val::Int(-5)),
            ("blocker_generation", Val::Null),
        ],
    )
}
fn native<'a>(row: &'a Rec, name: &str) -> &'a Val {
    row.columns().find(|(key, _)| *key == name).map(|(_, value)| value).unwrap()
}
fn healthy_resources() -> Rec {
    replace(
        &crate::resources::test_support::latest_row("2020-01-01T05:59:59Z"),
        &[
            ("observed_at", Val::Text("2020-01-01T06:00:00Z".into())),
            ("avg_cpu_percent", Val::Float(0.0)),
            ("avg_instance_cpu_percent", Val::Float(0.0)),
            ("max_worker_percent", Val::Float(0.0)),
            ("avg_data_io_percent", Val::Float(0.0)),
            ("avg_log_write_percent", Val::Float(0.0)),
        ],
    )
}

#[tokio::test]
async fn first_or_second_azure_pressure_deferral_stops_optional_work_without_more_sql_or_identity_probes() {
    for guard in [3, 6] {
        for (rows, stop) in [
            (vec![replace(&healthy_resources(), &[("avg_cpu_percent", Val::Float(80.0))])], CollectionStop::Pressure),
            (
                vec![replace(&healthy_resources(), &[("avg_instance_cpu_percent", Val::Null)])],
                CollectionStop::PressureUnavailable,
            ),
            (vec![], CollectionStop::PressureUnavailable),
        ] {
            let mut replies = successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
            replies[guard] = Ok(rows);
            let mut native = reader(replies);
            let error = native.capture_page_wait_targets().await.err().unwrap();
            assert_eq!(CollectionStop::from_error(&error), stop);
            assert_eq!(native.query_count(), guard as u32 + 1, "no SQL after a pressure deferral");
            assert_eq!(
                native.io.calls.iter().filter(|call| call.sql.contains("OUTER APPLY sys.dm_db_page_info")).count(),
                usize::from(guard == 6)
            );
            assert_eq!(native.close().await, SourceShutdown::Quiescent);
            assert_eq!(native.io.closed, 1);
        }
    }
    let mut replies = successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
    replies[3] = Ok(vec![]);
    let mut native = reader(replies);
    let (snapshot, coverage) = crate::page_waits::collect_from(&mut native).await;
    assert!(snapshot.is_none());
    assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::Unavailable);
    assert!(coverage.outcomes()[0].reason.as_ref().unwrap().contains("resource sample"));
    assert_eq!(native.query_count(), 4);
    assert_eq!(native.io.closed, 1);
}
pub(crate) fn participant(ordinal: usize, row: &Rec) -> Rec {
    let mut values = vec![
        ("ordinal", Val::Int(ordinal as i64)),
        ("observed_at", Val::Text("2020-01-01T06:00:00Z".into())),
        ("checked_plan_handle", Val::Text("CD".repeat(64))),
        ("checked_plan_hash", Val::Text("FEDCBA9876543210".into())),
    ];
    for (output, input) in [
        ("checked_session_id", "session_id"),
        ("checked_session_generation", "session_generation"),
        ("checked_request_id", "request_id"),
        ("checked_request_generation", "request_generation"),
        ("checked_sql_handle", "sql_handle"),
        ("checked_begin", "statement_start"),
        ("checked_end", "statement_end"),
        ("checked_database_id", "request_database_id"),
        ("checked_wait_type", "wait_type"),
        ("checked_wait_resource", "wait_resource"),
        ("checked_query_hash", "query_hash"),
    ] {
        values.push((output, native(row, input).clone()));
    }
    Rec::of(&values)
}
pub(crate) fn header(ordinal: usize, page: i64) -> Rec {
    let mut values = fields::NATIVE_FIELDS.iter().map(|name| (*name, Val::Null)).collect::<Vec<_>>();
    values.extend([
        ("ordinal", Val::Int(ordinal as i64)),
        ("observed_at", Val::Text("2020-01-01T06:00:00Z".into())),
        ("target_database_id", Val::Int(7)),
        ("target_file_id", Val::Int(1)),
        ("target_page_id", Val::Int(page)),
        ("file_guid", Val::Text("native-file-guid".into())),
        ("file_create_lsn", Val::Text("0".into())),
        ("file_type", Val::Int(0)),
        ("file_state", Val::Int(0)),
        ("catalog_object_id", Val::Int(9007199)),
        ("schema_name", Val::Text("dbo".into())),
        ("object_name", Val::Text("orders".into())),
        ("object_created_without_offset", Val::Text("2019-02-01T04:00:00".into())),
        ("object_modified_without_offset", Val::Text("2019-03-01T04:00:00".into())),
        ("catalog_index_id", Val::Int(1)),
        ("index_name", Val::Text("PK_orders".into())),
        ("index_type", Val::Int(1)),
        ("catalog_partition_id", Val::Int(9007199254740993)),
        ("catalog_hobt_id", Val::Int(9007199254740994)),
    ]);
    replace(
        &Rec::of(&values),
        &[
            ("database_id", Val::Int(7)),
            ("file_id", Val::Int(1)),
            ("page_id", Val::Int(page)),
            ("page_header_version", Val::Int(1)),
            ("page_type", Val::Int(1)),
            ("page_level", Val::Int(0)),
            ("object_id", Val::Int(9007199)),
            ("index_id", Val::Int(1)),
            ("partition_id", Val::Int(9007199254740993)),
            ("alloc_unit_id", Val::Int(9007199254740995)),
            ("page_lsn", Val::Text("00000001:00000002:0003".into())),
            ("next_page_file_id", Val::Int(0)),
            ("next_page_page_id", Val::Int(0)),
        ],
    )
}
pub(crate) fn successful_replies(rows: Vec<Rec>) -> Vec<ReadResult<Vec<Rec>>> {
    let participants = rows.iter().enumerate().map(|(index, row)| participant(index, row)).collect::<Vec<_>>();
    let addresses = rows
        .iter()
        .filter_map(|row| {
            if let Val::Text(resource) = native(row, "wait_resource") { PageAddress::parse(resource) } else { None }
        })
        .collect::<BTreeSet<_>>();
    let headers = addresses
        .iter()
        .enumerate()
        .map(|(ordinal, address)| header(ordinal, i64::from(address.page)))
        .collect::<Vec<_>>();
    let texts = rows
        .iter()
        .map(|row| {
            let Val::Int(id) = native(row, "session_id") else { panic!() };
            details(*id, "SELECT * FROM dbo.orders WHERE value=N'private-page-canary'")
        })
        .collect();
    vec![
        Ok(vec![context()]),
        Ok(rows),
        Ok(participants.clone()),
        Ok(vec![healthy_resources()]),
        Ok(headers.clone()),
        Ok(texts),
        Ok(vec![healthy_resources()]),
        Ok(headers),
        Ok(participants),
        Ok(vec![context()]),
    ]
}

#[tokio::test]
async fn short_positive_waits_share_page_lookups_without_claiming_a_holder_or_fetching_plans() {
    let rows = vec![page_wait(42, "PAGELATCH_EX", "7:1:4144"), page_wait(43, "PAGEIOLATCH_SH", "7:1:4144")];
    let mut reader = reader(successful_replies(rows));
    let capture = reader.capture_page_wait_targets().await.unwrap();
    assert_eq!(reader.query_count(), 10);
    assert!(capture.snapshot.is_valid());
    assert_eq!(capture.snapshot.observations.len(), 2);
    for row in &capture.snapshot.observations {
        assert_eq!(row.resolution, PageWaitResolution::ObservedAtBothBounds);
        assert_eq!(row.target.as_ref().unwrap().kind, PageWaitTargetKind::IndexLeaf);
        assert_eq!(row.target.as_ref().unwrap().object.as_ref().unwrap().index_name.as_deref(), Some("PK_orders"));
        assert_eq!(row.target.as_ref().unwrap().header.partition_id.as_deref(), Some("9007199254740993"));
        assert!(row.statement_identity.is_some());
        assert!(row.plan_identity.is_some());
        assert_eq!(row.request.client.as_ref().unwrap().application.as_deref(), Some("app"));
        assert_eq!(row.request.elapsed_ms.as_deref(), Some("1"));
    }
    assert_eq!(capture.snapshot.observations[0].resource.kind, WaitResourceKind::MemoryPage);
    assert_eq!(capture.snapshot.observations[1].resource.kind, WaitResourceKind::PageIo);
    assert!(!serde_json::to_string(&capture.snapshot).unwrap().contains("private-page-canary"));
    for index in [HEADER_BEFORE, HEADER_AFTER] {
        assert_eq!(reader.io.calls[index].limit, 1);
        assert!(reader.io.calls[index].sql.contains("'LIMITED'"));
    }
    for call in &reader.io.calls {
        assert!(call.sql.contains("/* culprit */"));
        assert!(!call.sql.contains("ORDER BY") || call.sql.contains("sys.dm_db_resource_stats"));
        for forbidden in [
            "DBCC",
            "dm_db_database_page_allocations",
            "dm_os_buffer_descriptors",
            "dm_exec_query_plan",
            "GROUP BY",
            "database_recovery_status",
        ] {
            assert!(!call.sql.contains(forbidden), "{forbidden}");
        }
    }
    assert!(reader.io.calls[1].sql.contains("r.database_id=DB_ID()"));
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(reader.io.closed, 1);
}

#[tokio::test]
async fn native_header_changes_and_object_reuse_withhold_mapping_but_not_the_original_wait() {
    for (column, value) in [
        ("alloc_unit_id", Val::Int(123)),
        ("partition_id", Val::Int(123)),
        ("file_guid", Val::Text("replacement-file".into())),
        ("file_create_lsn", Val::Text("123".into())),
        ("object_modified_without_offset", Val::Text("2020-01-01T05:00:00".into())),
        ("page_type", Val::Int(2)),
    ] {
        let mut replies = successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
        replies[HEADER_AFTER] = Ok(vec![replace(&header(0, 4144), &[(column, value)])]);
        let capture = reader(replies).capture_page_wait_targets().await.unwrap();
        let row = &capture.snapshot.observations[0];
        assert_eq!(row.resolution, PageWaitResolution::ChangedMetadata, "{column}");
        assert!(row.target.is_none());
        assert!(row.statement_identity.is_none());
        assert_eq!(row.request.wait.as_ref().unwrap().native_type.as_deref(), Some("PAGELATCH_EX"));
    }
}

#[tokio::test]
async fn legitimate_page_writes_do_not_turn_lsn_or_next_links_into_identity_guards() {
    let mut replies = successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
    replies[HEADER_AFTER] = Ok(vec![replace(
        &header(0, 4144),
        &[("page_lsn", Val::Text("00000002:00000003:0004".into())), ("next_page_page_id", Val::Int(5000))],
    )]);
    let capture = reader(replies).capture_page_wait_targets().await.unwrap();
    let row = &capture.snapshot.observations[0];
    assert_eq!(row.resolution, PageWaitResolution::ObservedAtBothBounds);
    assert_eq!(row.target.as_ref().unwrap().header.next_page_id, Some(5000));
}

#[tokio::test]
async fn recycled_sessions_requests_statements_and_plans_never_receive_a_page_mapping() {
    for (column, value) in [
        ("checked_session_id", Val::Null),
        ("checked_session_generation", Val::Text("2020-01-01T07:01:00".into())),
        ("checked_request_generation", Val::Text("2020-01-01T07:59:00".into())),
        ("checked_begin", Val::Int(2)),
        ("checked_sql_handle", Val::Text("EF".repeat(64))),
        ("checked_plan_handle", Val::Text("EF".repeat(64))),
        ("checked_wait_resource", Val::Text("7:1:999".into())),
        ("checked_query_hash", Val::Text("9999999999999999".into())),
    ] {
        let row = page_wait(42, "PAGELATCH_EX", "7:1:4144");
        let checked = replace(&participant(0, &row), &[(column, value)]);
        let mut replies = successful_replies(vec![row]);
        replies[CHECK_AFTER] = Ok(vec![checked]);
        let capture = reader(replies).capture_page_wait_targets().await.unwrap();
        let row = &capture.snapshot.observations[0];
        assert!(
            matches!(row.resolution, PageWaitResolution::ChangedRequest | PageWaitResolution::MissingRequest),
            "{column}"
        );
        assert!(row.target.is_none());
        assert!(row.plan_identity.is_none());
        assert_eq!(row.request.key.session.id, "42");
    }
}

#[tokio::test]
async fn unsupported_or_denied_page_sources_still_preserve_native_waits_without_header_reads() {
    for (column, value, resolution) in [
        ("page_info_available", Val::Int(0), PageWaitResolution::Unsupported),
        ("page_permission", Val::Int(0), PageWaitResolution::PermissionDenied),
    ] {
        let context = replace(&context(), &[(column, value)]);
        let mut reader = reader(vec![
            Ok(vec![context.clone()]),
            Ok(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]),
            Ok(vec![details(42, "SELECT 1")]),
            Ok(vec![context]),
        ]);
        let capture = reader.capture_page_wait_targets().await.unwrap();
        assert_eq!(capture.snapshot.observations[0].resolution, resolution);
        assert_eq!(reader.query_count(), 4);
        assert!(reader.io.calls.iter().all(|call| !call.sql.contains("OUTER APPLY sys.dm_db_page_info")));
    }
}

#[tokio::test]
async fn foreign_tempdb_and_unrecognized_addresses_never_reach_the_header_function() {
    for (address, resolution) in
        [("2:1:4144", PageWaitResolution::OutsideDatabase), ("7:bad:4", PageWaitResolution::UnrecognizedAddress)]
    {
        let mut reader = reader(vec![
            Ok(vec![context()]),
            Ok(vec![page_wait(42, "PAGELATCH_EX", address)]),
            Ok(vec![details(42, "SELECT 1")]),
            Ok(vec![context()]),
        ]);
        let capture = reader.capture_page_wait_targets().await.unwrap();
        assert_eq!(capture.snapshot.observations[0].resolution, resolution);
        assert_eq!(reader.query_count(), 4);
        assert!(reader.io.calls.iter().all(|call| !call.sql.contains("OUTER APPLY sys.dm_db_page_info")));
    }
}

#[tokio::test]
async fn distinct_page_and_participant_limits_are_partial_without_querying_overflow_addresses() {
    let rows =
        (0..9).map(|index| page_wait(42 + index, "PAGELATCH_EX", &format!("7:1:{}", 4144 + index))).collect::<Vec<_>>();
    let mut replies = successful_replies(rows.clone());
    let admitted = rows[..8].iter().enumerate().map(|(index, row)| participant(index, row)).collect::<Vec<_>>();
    replies[2] = Ok(admitted.clone());
    replies[CHECK_AFTER] = Ok(admitted);
    let headers = (0..8).map(|index| header(index, 4144 + index as i64)).collect::<Vec<_>>();
    replies[HEADER_BEFORE] = Ok(headers.clone());
    replies[HEADER_AFTER] = Ok(headers);
    let mut reader = reader(replies);
    let capture = reader.capture_page_wait_targets().await.unwrap();
    assert!(capture.limited);
    assert_eq!(capture.snapshot.observations.len(), 9);
    assert_eq!(capture.snapshot.observations[8].resolution, PageWaitResolution::Limit);
    assert_eq!(reader.io.calls[HEADER_BEFORE].limit, 8);
    let Parameter::Text(xml) = &reader.io.calls[HEADER_BEFORE].parameters[0] else { panic!() };
    assert!(!xml.contains("page=\"4152\""));

    let rows = (0..65).map(|index| page_wait(42 + index, "PAGELATCH_EX", "7:1:4144")).collect::<Vec<_>>();
    let mut replies = successful_replies(rows.clone());
    let admitted = rows[..64].iter().enumerate().map(|(index, row)| participant(index, row)).collect::<Vec<_>>();
    replies[2] = Ok(admitted.clone());
    replies[CHECK_AFTER] = Ok(admitted);
    replies[SQL_TEXT] = Ok((0..64).map(|index| details(42 + index, "SELECT 1")).collect());
    let mut reader = super::tests::reader(replies);
    let (snapshot, coverage) = crate::page_waits::collect_from(&mut reader).await;
    assert_eq!(snapshot.unwrap().observations.len(), 64);
    assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::Partial);
    assert!(matches!(&reader.io.calls[1].parameters[0], Parameter::Integer(65)));
    assert_eq!(reader.io.closed, 1);
}

#[tokio::test]
async fn an_empty_coordinator_capture_is_partial_and_does_not_read_pages_or_sql() {
    let mut reader = reader(vec![Ok(vec![context()]), Ok(vec![context()]), Ok(vec![context()])]);
    let (snapshot, coverage) = crate::page_waits::collect_from(&mut reader).await;
    assert!(snapshot.unwrap().observations.is_empty());
    assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::Partial);
    assert_eq!(reader.query_count(), 3);
    assert_eq!(reader.io.closed, 1);
}

#[tokio::test]
async fn no_plan_or_catalog_visibility_preserves_native_header_without_inventing_labels() {
    let row = page_wait(42, "PAGELATCH_EX", "7:1:4144");
    let checked =
        replace(&participant(0, &row), &[("checked_plan_handle", Val::Null), ("checked_plan_hash", Val::Null)]);
    let missing = replace(
        &header(0, 4144),
        &[
            ("catalog_object_id", Val::Null),
            ("catalog_index_id", Val::Null),
            ("catalog_partition_id", Val::Null),
            ("index_type", Val::Null),
            ("schema_name", Val::Null),
            ("object_name", Val::Null),
        ],
    );
    let mut replies = successful_replies(vec![row]);
    replies[2] = Ok(vec![checked.clone()]);
    replies[CHECK_AFTER] = Ok(vec![checked]);
    replies[HEADER_BEFORE] = Ok(vec![missing.clone()]);
    replies[HEADER_AFTER] = Ok(vec![missing]);
    let capture = reader(replies).capture_page_wait_targets().await.unwrap();
    let row = &capture.snapshot.observations[0];
    assert_eq!(row.resolution, PageWaitResolution::ObservedAtBothBounds);
    assert!(row.target.as_ref().unwrap().object.is_none());
    assert_eq!(row.target.as_ref().unwrap().kind, PageWaitTargetKind::Unclassified);
    assert!(row.plan_identity.is_none());
    assert_eq!(row.target.as_ref().unwrap().header.object_id.as_deref(), Some("9007199"));
}

#[tokio::test]
async fn unqualified_index_layouts_keep_header_facts_without_rowstore_leaf_claims() {
    for native_type in [Val::Null, Val::Int(5), Val::Int(6), Val::Int(7)] {
        let changed = replace(&header(0, 4144), &[("index_type", native_type)]);
        let mut replies = successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
        replies[HEADER_BEFORE] = Ok(vec![changed.clone()]);
        replies[HEADER_AFTER] = Ok(vec![changed]);
        let capture = reader(replies).capture_page_wait_targets().await.unwrap();
        let target = capture.snapshot.observations[0].target.as_ref().unwrap();
        assert_eq!(target.kind, PageWaitTargetKind::Unclassified);
        assert_eq!(target.header.page_type, Some(1));
        assert_eq!(target.header.index_id.as_deref(), Some("1"));
    }
}

#[tokio::test]
async fn missing_file_authority_and_null_page_headers_never_become_none_equals_none_proof() {
    for (columns, resolution) in [
        (vec![("file_guid", Val::Null)], PageWaitResolution::UnverifiedSource),
        (
            vec![("database_id", Val::Null), ("file_id", Val::Null), ("page_id", Val::Null)],
            PageWaitResolution::Unavailable,
        ),
    ] {
        let missing = replace(&header(0, 4144), &columns);
        let mut replies = successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
        replies[HEADER_BEFORE] = Ok(vec![missing.clone()]);
        replies[HEADER_AFTER] = Ok(vec![missing]);
        let capture = reader(replies).capture_page_wait_targets().await.unwrap();
        assert_eq!(capture.snapshot.observations[0].resolution, resolution);
        assert!(capture.snapshot.observations[0].target.is_none());
    }
}

#[tokio::test]
async fn pfs_allocation_pages_and_unknown_native_formats_do_not_become_last_leaf_claims() {
    for (native_type, kind) in [
        (11, PageWaitTargetKind::Allocation),
        (10, PageWaitTargetKind::Allocation),
        (99, PageWaitTargetKind::Unclassified),
    ] {
        let changed = replace(&header(0, 4144), &[("page_type", Val::Int(native_type))]);
        let mut replies = successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
        replies[HEADER_BEFORE] = Ok(vec![changed.clone()]);
        replies[HEADER_AFTER] = Ok(vec![changed]);
        let capture = reader(replies).capture_page_wait_targets().await.unwrap();
        assert_eq!(capture.snapshot.observations[0].target.as_ref().unwrap().kind, kind);
    }
}

#[tokio::test]
async fn cancellation_deadline_and_budget_stops_forbid_further_native_work_and_close_withholds_fresh_data() {
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::ByteLimit,
        CollectionStop::QueryLimit,
        CollectionStop::Pressure,
    ] {
        let mut reader = reader(vec![
            Ok(vec![context()]),
            Ok(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]),
            Err(ReadError::Stopped(stop)),
        ]);
        let (snapshot, coverage) = crate::page_waits::collect_from(&mut reader).await;
        assert!(snapshot.is_none());
        assert_eq!(reader.query_count(), 3);
        assert_eq!(reader.io.closed, 1);
        assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::Unavailable);
    }
    let mut reader = reader(successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]));
    reader.io.shutdown = SourceShutdown::Unconfirmed;
    let (snapshot, coverage) = crate::page_waits::collect_from(&mut reader).await;
    assert!(snapshot.is_none());
    assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::Unavailable);
    assert_eq!(reader.io.closed, 1);
}

#[tokio::test]
async fn changed_database_epoch_and_regressed_native_clock_cannot_qualify_metadata() {
    for (column, value) in [
        ("physical_incarnation", Val::Text("replacement-physical".into())),
        ("database_created_without_offset", Val::Text("2020-01-01T04:00:00".into())),
        ("observed_at", Val::Text("2020-01-01T05:59:59Z".into())),
    ] {
        let mut replies = successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
        replies[FINAL_CONTEXT] = Ok(vec![replace(&context(), &[(column, value)])]);
        let error = reader(replies).capture_page_wait_targets().await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{column}");
    }
}

#[test]
fn address_and_native_field_decoding_preserve_signed_ids_and_reject_overflow_or_partial_headers() {
    for native in ["0:1:1", "7:0:1", "32768:1:1", "7:1:2147483648", "7:1:1:2", "7:1:-1", "7: 1:2"] {
        assert!(PageAddress::parse(native).is_none(), "{native}");
    }
    let address = PageAddress::parse("7:1:0").unwrap();
    assert_eq!(address.page, 0);
    let negative = replace(&header(0, 0), &[("object_id", Val::Int(-9007199)), ("alloc_unit_id", Val::Int(i64::MIN))]);
    let decoded = HeaderRecord::decode(&negative).unwrap();
    assert_eq!(decoded.header.object_id, Some(-9007199));
    assert_eq!(decoded.header.allocation_unit_id.as_deref(), Some("-9223372036854775808"));
    assert!(HeaderRecord::decode(&replace(&negative, &[("page_id", Val::Int(i64::MAX))])).is_err());
    let partial = HeaderRecord::decode(&replace(&header(0, 0), &[("file_id", Val::Null)])).unwrap();
    assert!(address.matches(&partial.header).is_err());
    let projection = sql::headers();
    for field in fields::NATIVE_FIELDS {
        assert!(projection.contains(&format!("p.{field}")), "{field}");
    }
}
