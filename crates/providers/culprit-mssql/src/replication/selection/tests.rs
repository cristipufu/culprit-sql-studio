use super::*;
use crate::{
    read::ReadError,
    replication::tests::{availability, geo, layout, primary, unavailable},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::SourceState;

#[tokio::test]
async fn native_platform_selects_the_same_bounded_ag_and_geo_families_without_error_fallback() {
    for edition in [2, 3, 4, 5, 8] {
        let mut replies = Vec::new();
        if edition != 5 {
            replies.extend([Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![primary(), availability()])]);
        }
        if matches!(edition, 5 | 8) {
            replies.push(Ok(vec![geo()]));
        }
        let expected_queries = replies.len();
        let mut reader = FakeRead::new(replies);
        let read = read_selected(&mut reader, edition, 10).await.unwrap();
        let (records, coverage) = read.into_records(true, None);
        assert_eq!(reader.calls.len(), expected_queries);
        assert!(reader.calls.iter().all(|call| !call.sql.contains("sys.dm_hs_database_replicas")));
        assert_eq!(coverage.len(), if edition == 8 { 2 } else { 1 });
        assert!(coverage.iter().all(|source| source.state == SourceState::Collected && source.complete_scope));
        assert_eq!(
            records.len(),
            if edition == 5 {
                1
            } else if edition == 8 {
                3
            } else {
                2
            }
        );
        assert_eq!(
            reader.calls.iter().any(|call| call.sql.contains("sys.dm_geo_replication_link_status")),
            matches!(edition, 5 | 8)
        );
        assert_eq!(
            reader.calls.iter().any(|call| call.sql.contains("sys.dm_hadr_database_replica_states drs")),
            edition != 5
        );
    }
    let mut unknown = FakeRead::new(vec![]);
    assert_eq!(read_selected(&mut unknown, 6, 10).await.err().unwrap().code, ErrorCode::Unsupported);
    assert_eq!(unknown.queries(), 0);
}

#[tokio::test]
async fn managed_instance_reads_its_supported_ag_interface_when_the_boxed_hadr_property_is_unavailable() {
    for hadr in [Val::Null, Val::Int(0)] {
        let mut reader =
            FakeRead::new(vec![Ok(vec![layout(hadr, 1)]), Ok(vec![primary(), availability()]), Ok(vec![geo()])]);
        let (records, coverage) = read_selected(&mut reader, 8, 10).await.unwrap().into_records(true, None);
        assert_eq!(records.len(), 3);
        assert!(coverage.iter().all(|source| source.state == SourceState::Collected && source.complete_scope));
        assert_eq!(reader.queries(), 3, "the existing layout and two native families, without a fallback query");
    }
}

#[tokio::test]
async fn managed_instance_retains_the_other_native_family_only_after_a_completed_permission_or_unsupported_failure() {
    for denied_ag in [false, true] {
        for (number, code) in [(229, ErrorCode::PermissionDenied), (208, ErrorCode::Unsupported)] {
            let denied = Ok(vec![unavailable(number)]);
            let replies = if denied_ag {
                vec![Ok(vec![layout(Val::Null, 1)]), denied, Ok(vec![geo()])]
            } else {
                vec![Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![primary(), availability()]), denied]
            };
            let mut reader = FakeRead::new(replies);
            let (records, coverage) = read_selected(&mut reader, 8, 10).await.unwrap().into_records(true, None);
            assert_eq!(coverage.len(), 2);
            let failed = &coverage[usize::from(!denied_ag)];
            assert_eq!(failed.state, SourceState::Unavailable);
            assert_eq!(failed.error_code, Some(code));
            assert!(!failed.complete_scope);
            assert_eq!(coverage[usize::from(denied_ag)].state, SourceState::Collected);
            assert_eq!(records.len(), if denied_ag { 1 } else { 2 });
        }
    }
}

#[tokio::test]
async fn terminal_stops_and_malformed_rows_prevent_all_later_replication_queries() {
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
        CollectionStop::SourceChanged,
    ] {
        let mut reader =
            FakeRead::new(vec![Ok(vec![layout(Val::Int(1), 1)]), Err(ReadError::Stopped(stop)), Ok(vec![geo()])]);
        let error = read_selected(&mut reader, 8, 10).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.queries(), 2);
    }
    let malformed = replace(&availability(), &[("is_primary", Val::Int(2))]);
    let mut reader = FakeRead::new(vec![Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![malformed]), Ok(vec![geo()])]);
    assert!(read_selected(&mut reader, 8, 10).await.is_err());
    assert_eq!(reader.queries(), 2, "malformed authority cannot authorize a different source query");
    for code in [ErrorCode::PermissionDenied, ErrorCode::Unsupported] {
        let mut reader = FakeRead::new(vec![
            Ok(vec![layout(Val::Null, 1)]),
            Err(ReadError::Source(Error::new(code, "controlled retired native owner"))),
            Ok(vec![geo()]),
        ]);
        assert_eq!(read_selected(&mut reader, 8, 10).await.err().unwrap().code, code);
        assert_eq!(reader.queries(), 2, "an error outside the native availability protocol cannot reopen an owner");
    }
}

#[tokio::test]
async fn missing_native_role_columns_and_secondary_visibility_keep_raw_facts_partial() {
    for edition in [3, 8] {
        let old_layout = replace(&layout(Val::Int(1), 0), &[("primary_column", Val::Int(0))]);
        let old_row = replace(&availability(), &[("is_primary", Val::Null), ("secondary_lag_seconds", Val::Null)]);
        let mut replies = vec![Ok(vec![old_layout]), Ok(vec![old_row])];
        if edition == 8 {
            replies.push(Ok(vec![geo()]));
        }
        let mut reader = FakeRead::new(replies);
        let (records, coverage) = read_selected(&mut reader, edition, 10).await.unwrap().into_records(true, None);
        assert!(!reader.calls[1].sql.contains("drs.is_primary_replica"));
        assert!(!reader.calls[1].sql.contains("drs.secondary_lag_seconds"));
        assert_eq!(coverage[0].state, SourceState::Partial);
        let TelemetryRecord::Replication(row) = &records[0] else { panic!() };
        assert!(application(&row.fact).is_none());
        if edition == 8 {
            assert_eq!(coverage[1].state, SourceState::Collected, "AG column visibility does not erase Geo facts");
        }
    }

    let secondary = replace(&availability(), &[("is_local", Val::Int(1))]);
    let mut reader = FakeRead::new(vec![Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![secondary])]);
    let (records, coverage) = read_selected(&mut reader, 3, 10).await.unwrap().into_records(true, None);
    assert_eq!(coverage[0].state, SourceState::Partial);
    assert!(!coverage[0].complete_scope);
    let TelemetryRecord::Replication(row) = &records[0] else { panic!() };
    assert_eq!(application(&row.fact).unwrap().role, ReplicaRole::ThisServer);
}

#[tokio::test]
async fn source_empty_requires_success_and_unknown_reader_qualification_never_publishes_complete_absence() {
    let mut reader = FakeRead::new(vec![Ok(vec![layout(Val::Int(0), 0)])]);
    let (records, coverage) = read_selected(&mut reader, 3, 10).await.unwrap().into_records(true, None);
    assert!(records.is_empty());
    assert_eq!(reader.queries(), 1);
    assert_eq!(coverage[0].state, SourceState::Empty);
    assert!(coverage[0].complete_scope);
    let mut reader = FakeRead::new(vec![Ok(vec![])]);
    let (records, coverage) =
        read_selected(&mut reader, 5, 10).await.unwrap().into_records(false, Some(ErrorCode::PermissionDenied));
    assert!(records.is_empty());
    assert_eq!(coverage[0].state, SourceState::Partial);
    assert!(!coverage[0].complete_scope);
    assert_eq!(coverage[0].error_code, Some(ErrorCode::PermissionDenied));
}

#[tokio::test]
async fn the_second_native_family_has_only_the_original_remaining_row_allowance() {
    let mut full =
        FakeRead::new(vec![Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![primary(), availability()]), Ok(vec![geo()])]);
    let error = read_selected(&mut full, 8, 2).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    assert_eq!(full.queries(), 2, "no zero-budget geo query or allowance reset");
    let mut second = FakeRead::new(vec![Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![primary()]), Ok(vec![geo()])]);
    read_selected(&mut second, 8, 3).await.unwrap();
    assert_eq!(second.calls[1].limit, 4);
    assert_eq!(second.calls[2].limit, 3, "two remaining rows plus one overflow witness");
}
