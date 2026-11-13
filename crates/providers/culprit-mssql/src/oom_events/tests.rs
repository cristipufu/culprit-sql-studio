use super::*;
use crate::{resources::test_support::FakeRead, value::Val};

pub(crate) fn platform(edition: i64) -> Rec {
    Rec::of(&[("edition", Val::Int(edition))])
}
pub(crate) fn row() -> Rec {
    Rec::of(&[
        (EVENT_TIME, Val::Text("2026-11-13T05:13:01.003".into())),
        (CAUSE, Val::Int(11)),
        (CAUSE_DESCRIPTION, Val::Text("QUERY_MEMORY_GRANTS".into())),
        (FACTOR, Val::Int(10)),
        (FACTOR_DESCRIPTION, Val::Text("QUERY_MEM_QUEUE".into())),
        ("available_physical_memory_mb", Val::Int(0)),
        ("process_memory_usage_mb", Val::Int(1024)),
        ("non_sos_memory_usage_mb", Val::Int(12)),
        ("committed_memory_target_mb", Val::Int(900)),
        ("committed_memory_mb", Val::Int(950)),
        ("allocation_potential_memory_mb", Val::Int(-50)),
        ("initial_job_object_memory_limit_mb", Val::Null),
        ("current_job_object_memory_limit_mb", Val::Int(1200)),
        ("possible_non_sos_leaked_memory_mb", Val::Null),
        ("oom_resource_pools", Val::Null),
        (
            "top_memory_clerks",
            Val::Text("[{\"clerk_type_name\":\"CACHESTORE_SQLCP\",\"future\":9007199254740993}]".into()),
        ),
        ("top_resource_pools", Val::Text("null".into())),
        ("possible_leaked_memory_clerks", Val::Text("{broken".into())),
    ])
}
pub(crate) fn replace(row: Rec, column: &str, value: Val) -> Rec {
    Rec::of(
        &row.columns()
            .map(|(key, old)| (key, if key == column { value.clone() } else { old.clone() }))
            .collect::<Vec<_>>(),
    )
}

#[tokio::test]
async fn supported_read_preserves_native_duplicates_signed_values_nulls_and_raw_json() {
    for edition in [5, 8] {
        let mut reader = FakeRead::new(vec![Ok(vec![platform(edition)]), Ok(vec![row(), row()])]);
        let facts = read(&mut reader, 10).await.unwrap().unwrap();
        assert_eq!(facts.iter().map(|row| row.position).collect::<Vec<_>>(), [0, 1]);
        assert_ne!(facts[0].key().unwrap(), facts[1].key().unwrap());
        let fact = &facts[0];
        assert_eq!(fact.allocation_potential_memory_mb, -50);
        assert_eq!(fact.available_physical_memory_mb, 0);
        assert_eq!(fact.initial_job_object_memory_limit_mb, None);
        assert_eq!(fact.current_job_object_memory_limit_mb, Some(1200));
        assert_eq!(fact.oom_resource_pools, None);
        assert_eq!(fact.top_resource_pools.as_ref().unwrap().raw, "null");
        assert_eq!(
            fact.possible_leaked_memory_clerks.as_ref().unwrap().qualification,
            MemoryFailureJsonQualification::Unparsed
        );
        assert!(fact.top_memory_clerks.as_ref().unwrap().raw.contains("9007199254740993"));
        assert_eq!(fact.event_time_without_offset, "2026-11-13T05:13:01.003");
        assert_eq!(reader.calls.len(), 2);
        assert_eq!(reader.calls[1].limit, 11);
        assert!(matches!(reader.calls[1].parameters.as_slice(), [Parameter::Integer(11)]));
        for forbidden in ["ORDER BY", "GROUP BY", "OPENJSON", "JOIN", "dm_os_ring_buffers", "dm_xe_"] {
            assert!(!reader.calls[1].sql.contains(forbidden));
        }
        assert!(reader.calls[1].sql.contains("/* culprit */") && reader.calls[1].sql.contains("MAXDOP 1"));
        assert_eq!(reader.closed, 0, "the borrowing reader cannot close its caller's native session");
    }
}

#[tokio::test]
async fn boxed_products_never_probe_the_unsupported_dmv_even_if_it_exists() {
    for edition in [2, 3, 4] {
        let mut reader = FakeRead::new(vec![Ok(vec![platform(edition)])]);
        assert!(read(&mut reader, 10).await.unwrap().is_none());
        assert_eq!(reader.calls.len(), 1);
        assert!(!reader.calls[0].sql.contains(NATIVE_OBJECT));
    }
    let mut reader = FakeRead::new(vec![Ok(vec![platform(99)])]);
    assert_eq!(read(&mut reader, 10).await.unwrap_err().code, ErrorCode::Unsupported);
    assert_eq!(reader.calls.len(), 1);
}

#[tokio::test]
async fn source_bounds_fail_explicitly_instead_of_dropping_or_merging_events() {
    let mut reader = FakeRead::new(vec![]);
    assert_eq!(CollectionStop::from_error(&read(&mut reader, 0).await.unwrap_err()), CollectionStop::RowLimit);
    assert!(reader.calls.is_empty());
    let mut reader = FakeRead::new(vec![Ok(vec![platform(5)]), Ok(vec![row(), row()])]);
    assert_eq!(CollectionStop::from_error(&read(&mut reader, 1).await.unwrap_err()), CollectionStop::RowLimit);
    assert_eq!(reader.calls.len(), 2);
    assert!(decode(&row(), MAX_MEMORY_FAILURE_OBSERVATIONS).is_err());
}

#[test]
fn native_field_failures_cannot_become_zero_or_clipped_json() {
    for (column, value) in [
        (CAUSE, Val::Int(256)),
        (FACTOR, Val::Null),
        ("process_memory_usage_mb", Val::Null),
        ("process_memory_usage_mb", Val::Int(i64::from(i32::MAX) + 1)),
        ("current_job_object_memory_limit_mb", Val::Float(1.0)),
        (EVENT_TIME, Val::Text("2026-11-13T05:13:01Z".into())),
        ("top_memory_clerks", Val::Text("x".repeat(MAX_MEMORY_FAILURE_JSON_UTF16 + 1))),
    ] {
        assert!(decode(&replace(row(), column, value), 0).is_err(), "{column}");
    }
    assert!(decode(&Rec::default(), 0).is_err());
    let future =
        replace(replace(row(), CAUSE, Val::Int(255)), CAUSE_DESCRIPTION, Val::Text("FUTURE_NATIVE_CAUSE".into()));
    assert_eq!(decode(&future, 0).unwrap().oom_cause, 255);
}

#[tokio::test]
async fn permission_cancellation_and_timeout_remain_original_failures_without_fallback() {
    for stop in [
        CollectionStop::TimedOut,
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![platform(5)]), Err(crate::read::ReadError::Stopped(stop))]);
        assert_eq!(CollectionStop::from_error(&read(&mut reader, 10).await.unwrap_err()), stop);
        assert_eq!(reader.calls.len(), 2);
    }
    let mut reader = FakeRead::new(vec![
        Ok(vec![platform(8)]),
        Err(Error::new(ErrorCode::PermissionDenied, "native OOM view denied").into()),
    ]);
    assert_eq!(read(&mut reader, 10).await.unwrap_err().code, ErrorCode::PermissionDenied);
    assert_eq!(reader.calls.len(), 2);
}
